import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useMemo, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import {
  Button,
  Checkbox,
  Layout,
  Space,
  Tooltip,
  Typography,
} from '@arco-design/web-react'
import { listen, UnlistenFn } from '@tauri-apps/api/event'
import { Bookmark, Play, Square } from 'lucide-react'
import {
  addMarker,
  deleteMarker,
  Device,
  EVENT_SAMPLE,
  EVENT_SESSION_ENDED,
  listApps,
  listDevices,
  Marker,
  startSession,
  stopSession,
  updateMarker,
  updateMarkerLabel,
} from '@/lib/ipc'
import { MarkerControls } from '@/lib/markers'
import { LiveCpuChart } from '@/components/LiveCpuChart'
import { LivePerCoreChart } from '@/components/LivePerCoreChart'
import { LiveFpsChart } from '@/components/LiveFpsChart'
import { LiveGpuChart } from '@/components/LiveGpuChart'
import { LiveMemoryChart } from '@/components/LiveMemoryChart'
import { LiveTemperatureChart } from '@/components/LiveTemperatureChart'
import { DeviceSelector, deviceFromKey, deviceKey } from '@/components/DeviceSelector'
import { AppSelector } from '@/components/AppSelector'
import { LogTerminal } from '@/components/LogTerminal'
import { SidebarTabs } from '@/components/SidebarTabs'
import { useResizableSidebar } from '@/lib/useResizableSidebar'
import chartStyles from '@/components/chart-shared.module.scss'

const { Sider, Content } = Layout
const { Title, Text } = Typography

export type Notice = {
  kind: 'info' | 'success' | 'warning' | 'error'
  text: string
  auto?: boolean
}

export function LiveView({
  activeSessionId,
  setActiveSessionId,
}: {
  activeSessionId: number | null
  setActiveSessionId: (id: number | null) => void
}) {
  const qc = useQueryClient()
  const { data, isLoading } = useQuery({
    queryKey: ['devices'],
    queryFn: listDevices,
    // 1.5s cadence: `adb devices` + usbmuxd `get_devices` are both local
    // IPC and finish in <50ms; the device-name lockdown query is cached
    // for 15 min so it doesn't run on every poll. Faster polling makes
    // the disconnect watchdog feel near-instant.
    refetchInterval: 1500,
  })

  /// Compound key (platform:id:transport) — same UDID may surface twice
  /// on iOS (USB + Wi-Fi); the sampler-relevant identity is id+transport.
  const [selectedKey, setSelectedKey] = useState<string | null>(null)
  const selected: Device | null = useMemo(
    () => (selectedKey && data ? deviceFromKey(selectedKey, data) ?? null : null),
    [selectedKey, data],
  )
  const recording = activeSessionId != null
  const [targetPkg, setTargetPkg] = useState<string | null>(null)
  const sidebar = useResizableSidebar()
  /// Log terminal toggle + resizable height. Default closed (240px
  /// reserved height makes the chart area feel cramped on first
  /// launch — opt-in is the right default). Persisted in localStorage
  /// so the panel size survives across launches.
  const [logOpen, setLogOpen] = useState(false)
  const [logHeight, setLogHeight] = useState(() => {
    const raw = typeof window !== 'undefined' ? localStorage.getItem('mperf.logHeight') : null
    const n = raw == null ? NaN : Number(raw)
    return Number.isFinite(n) ? Math.max(120, Math.min(600, n)) : 240
  })
  const [logDragging, setLogDragging] = useState(false)
  const logDragStartRef = useRef<{ y: number; h: number }>({ y: 0, h: 240 })
  const onLogHandlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    logDragStartRef.current = { y: e.clientY, h: logHeight }
    setLogDragging(true)
    ;(e.target as HTMLDivElement).setPointerCapture(e.pointerId)
  }
  const onLogHandlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!logDragging) return
    // Drag up = grow the panel (we're at the top edge of the panel).
    const dy = e.clientY - logDragStartRef.current.y
    const next = Math.max(120, Math.min(600, logDragStartRef.current.h - dy))
    setLogHeight(next)
  }
  const onLogHandlePointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!logDragging) return
    setLogDragging(false)
    try {
      localStorage.setItem('mperf.logHeight', String(logHeight))
    } catch {
      // ignore
    }
    const el = e.target as HTMLDivElement
    if (el.hasPointerCapture?.(e.pointerId)) el.releasePointerCapture(e.pointerId)
  }
  /// Markers dropped in the current recording. Cleared on session start.
  /// Charts read this via prop and render vertical lines through it.
  const [markers, setMarkers] = useState<Marker[]>([])
  /// Live override for a marker being dragged on a chart. While the
  /// user is mid-drag, every chart needs to render the marker at the
  /// new (provisional) position — we substitute this entry into the
  /// `markers` array we hand down. Cleared on drag end.
  const [dragMarker, setDragMarker] = useState<{ id: number; ts_us: number } | null>(null)
  const effectiveMarkers = dragMarker
    ? markers.map((m) =>
        m.id === dragMarker.id ? { ...m, ts_us: dragMarker.ts_us } : m,
      )
    : markers
  /// State-based notification. Arco 2.66's `Message` / `Notification`
  /// crash on React 19 (`ReactDOM.render` was removed), so we surface
  /// every toast-equivalent message via a rendered banner instead.
  /// `auto` dismisses on its own; otherwise the user clicks Dismiss.
  const [notice, setNotice] = useState<Notice | null>(null)

  // Auto-dismiss success/info banners after a few seconds so they
  // behave like the toast they're replacing.
  useEffect(() => {
    if (!notice || !notice.auto) return
    const id = setTimeout(() => setNotice(null), 3500)
    return () => clearTimeout(id)
  }, [notice])

  const appsQuery = useQuery({
    queryKey: ['apps', selected?.id, selected?.platform],
    queryFn: () =>
      selected ? listApps(selected.id, selected.platform) : Promise.resolve([]),
    enabled: !!selected,
    staleTime: 30_000,
  })

  const handleStart = async () => {
    if (!selected || !targetPkg) return
    setNotice(null)
    setMarkers([])
    try {
      const sid = await startSession(
        selected.id,
        selected.platform,
        targetPkg,
        selected.model,
      )
      setActiveSessionId(sid)
      setNotice({ kind: 'success', text: `Session #${sid} recording`, auto: true })
    } catch (e) {
      setNotice({ kind: 'error', text: String(e) })
    }
  }

  const handleStop = async () => {
    try {
      await stopSession()
    } finally {
      setActiveSessionId(null)
      qc.invalidateQueries({ queryKey: ['sessions'] })
    }
  }

  const handleAddMarker = async () => {
    if (!recording) return
    try {
      const m = await addMarker()
      // Append in chronological order — backend assigns ts_us monotonically.
      setMarkers((prev) => [...prev, m])
    } catch (e) {
      setNotice({ kind: 'error', text: `add marker failed: ${e}` })
    }
  }

  const handleMarkerDragMove = (id: number, tsUs: number) => {
    setDragMarker({ id, ts_us: tsUs })
  }

  const handleMarkerDragEnd = (id: number, tsUs: number) => {
    // Optimistic local update so the chart stays at the new position
    // without flashing back to the old one before the backend round-trip
    // completes.
    setMarkers((prev) =>
      prev.map((m) => (m.id === id ? { ...m, ts_us: tsUs } : m)),
    )
    setDragMarker(null)
    void updateMarker(id, tsUs).catch((e) => {
      setNotice({ kind: 'error', text: `marker move failed: ${e}` })
    })
  }

  const handleMarkerDelete = (id: number) => {
    setMarkers((prev) => prev.filter((m) => m.id !== id))
    void deleteMarker(id).catch((e) => {
      setNotice({ kind: 'error', text: `marker delete failed: ${e}` })
    })
  }

  const handleMarkerLabelEdit = (id: number, label: string | null) => {
    setMarkers((prev) =>
      prev.map((m) => (m.id === id ? { ...m, label } : m)),
    )
    void updateMarkerLabel(id, label).catch((e) => {
      setNotice({ kind: 'error', text: `marker label save failed: ${e}` })
    })
  }

  const markerControls: MarkerControls = {
    list: effectiveMarkers,
    onDragMove: handleMarkerDragMove,
    onDragEnd: handleMarkerDragEnd,
    onDelete: handleMarkerDelete,
    onLabelEdit: handleMarkerLabelEdit,
  }

  // Cmd/Ctrl + Shift + M: drop a marker at the current instant. Chosen
  // over Cmd+M because Cmd+M is "minimize window" on macOS.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (
        (e.metaKey || e.ctrlKey) &&
        e.shiftKey &&
        (e.key === 'm' || e.key === 'M')
      ) {
        e.preventDefault()
        void handleAddMarker()
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
    // handleAddMarker captures `recording`; recreating on change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recording])

  // Diagnostic: log every devices fetch so we can see if/when the
  // disconnected device drops out of usbmuxd's list.
  useEffect(() => {
    if (data) {
      console.log(
        '[mperf] list_devices →',
        data.map(
          (d) => `${d.platform}:${d.id.slice(0, 12)}…(${d.state}, usable=${d.usable})`,
        ),
      )
    }
  }, [data])

  // `selected` is a `useMemo` derivation of `selectedKey + data` (see
  // declaration above) — it auto-refreshes when the next list_devices
  // poll changes the underlying entry's `state` / `usable` / `model`, so
  // we don't need a sync effect.

  // Backend ends the session itself on USB disconnect / fatal sampler
  // error. Surface that to the UI so the Stop button doesn't get stuck.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined
    let cancelled = false
    listen<{ session_id: number; reason: string }>(EVENT_SESSION_ENDED, (e) => {
      console.log('[mperf] EVENT_SESSION_ENDED', e.payload)
      setActiveSessionId(null)
      qc.invalidateQueries({ queryKey: ['sessions'] })
      setNotice({
        kind: 'warning',
        text: `Session #${e.payload.session_id} ended automatically (reason: ${e.payload.reason}).`,
        auto: true,
      })
    }).then((fn) => {
      if (cancelled) fn()
      else unlisten = fn
    })
    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [qc, setActiveSessionId])

  // Watchdog: detect device disconnect by polling list_devices. iOS's DTX
  // channel can hang on USB unplug so we can't rely on the backend
  // sampler erroring out — the frontend has to notice.
  //
  // Detection is done on `usable` (the backend-computed flag — true iff
  // we can actually sample the device), not raw `state` strings. Why:
  // iOS USB renegotiation can briefly flicker the device through
  // network mode for a single 3-second poll cycle, and string-matching
  // "usb" trips on that and kills the session. We require **two
  // consecutive** non-usable polls (~6s elapsed) to confirm a real
  // disconnect, which still feels instant in practice.
  //
  // Subtle dep-array choice: only `selected?.id` (not the whole
  // `selected`) is in the deps. The earlier sync effect calls
  // `setSelected(fresh)` whenever the device's state/usable/model
  // flips — if we put `selected` in deps, the watchdog would re-run on
  // that re-render, see `current.usable === false` again, and either
  // strike a second time (wrong: same poll counted twice) or, with the
  // pre-fix condition `selected.usable && !current.usable`, find
  // `selected.usable` already flipped to false and reset strikes to 0
  // (wrong: 2-poll requirement bypassed, watchdog never fires).
  // Re-running only when the underlying `data` changes preserves the
  // "once per 3s poll" cadence the strike counter assumes.
  const disconnectStrikesRef = useRef(0)
  useEffect(() => {
    if (!recording || !selectedKey || !data) {
      disconnectStrikesRef.current = 0
      return
    }
    // Lookup uses the compound key — same UDID may exist on USB and
    // Wi-Fi simultaneously on iOS, and pulling USB leaves only the
    // Wi-Fi entry behind. We treat "the *specific* entry the user
    // picked is gone" as the disconnect signal, not "any entry with
    // matching id".
    const current = data.find((d) => deviceKey(d) === selectedKey)
    // Two disconnect signals with different debounce needs:
    //   1. `!current` — the device is entirely gone from the list. adb /
    //      usbmuxd doesn't lose a still-attached device for a single
    //      poll, so this is a deterministic signal; fire immediately.
    //   2. `!current.usable` — device is still listed but reports
    //      `state=offline` (Android) or transport=Wifi (iOS). iOS USB
    //      renegotiation briefly flickers the device through Wi-Fi mode
    //      for a single poll cycle, so keep the 2-strike debounce on
    //      this path to avoid false-positive session kills.
    // Verbose poll-by-poll log to help diagnose "device looked present
    // when it shouldn't be" cases. Stripped only if the device looks fine
    // (the common idle case) — first-strike / disconnect already log.
    if (!current || !current.usable) {
      console.log(
        '[mperf] watchdog poll: selectedKey=%s present=%s usable=%s, devices=[%s]',
        selectedKey,
        !!current,
        current?.usable,
        data.map((d) => `${deviceKey(d)}(usable=${d.usable},state=${d.state})`).join(', '),
      )
    }
    if (!current) {
      disconnectStrikesRef.current = 0
      const reason = 'gone from device list'
      console.warn(`[perfdog] watchdog: ${selectedKey} — ${reason}`)
      setNotice({
        kind: 'warning',
        text: `Device disconnected (${reason}) — stopping the session.`,
        auto: true,
      })
      void stopSession().finally(() => {
        setActiveSessionId(null)
        qc.invalidateQueries({ queryKey: ['sessions'] })
      })
    } else if (!current.usable) {
      const reason = `lost USB connection (state=${current.state})`
      disconnectStrikesRef.current += 1
      if (disconnectStrikesRef.current < 2) {
        console.warn(
          `[perfdog] watchdog strike ${disconnectStrikesRef.current}/2 — ${reason}`,
        )
        return
      }
      disconnectStrikesRef.current = 0
      console.warn(`[perfdog] watchdog: ${selectedKey} — ${reason}`)
      setNotice({
        kind: 'warning',
        text: `Device disconnected (${reason}) — stopping the session.`,
        auto: true,
      })
      void stopSession().finally(() => {
        setActiveSessionId(null)
        qc.invalidateQueries({ queryKey: ['sessions'] })
      })
    } else {
      disconnectStrikesRef.current = 0
    }
  }, [data, recording, selectedKey, qc, setActiveSessionId])

  // Sample-heartbeat watchdog: a defense-in-depth catch for the cases
  // the list_devices watchdog can't see — adb keeping a yanked device
  // on the list as `state=device` briefly, usbmuxd holding a stale
  // entry, an iOS sampler silently hanging on a broken DTX read, or
  // any other path where the backend stops pushing samples but doesn't
  // emit EVENT_SESSION_ENDED. If we go HEARTBEAT_TIMEOUT_MS without a
  // single sample arriving while recording, we treat the session as
  // dead and run the same stop path the user's Stop button does.
  // Threshold tuned for: most samplers are 1Hz; battery + temp on
  // Android are 0.5Hz (2s period). 12s is well past both with comfortable
  // headroom for adb hiccups while still feeling responsive.
  const HEARTBEAT_TIMEOUT_MS = 12_000
  const lastSampleAtRef = useRef<number>(0)
  useEffect(() => {
    if (!recording) return
    lastSampleAtRef.current = Date.now()
    let cancelled = false
    let unlisten: UnlistenFn | undefined
    listen(EVENT_SAMPLE, () => {
      lastSampleAtRef.current = Date.now()
    }).then((fn) => {
      if (cancelled) fn()
      else unlisten = fn
    })
    const interval = setInterval(() => {
      const since = Date.now() - lastSampleAtRef.current
      if (since < HEARTBEAT_TIMEOUT_MS) return
      console.warn(
        `[perfdog] heartbeat: no samples for ${Math.round(since / 1000)}s — stopping session`,
      )
      setNotice({
        kind: 'warning',
        text: `No samples received for ${Math.round(
          since / 1000,
        )}s — stopping the session (device likely disconnected).`,
        auto: true,
      })
      // stopSession is idempotent on the backend (session is taken
      // out of the lock); resetting activeSessionId in finally also
      // tears down this interval via the !recording guard above.
      void stopSession().finally(() => {
        setActiveSessionId(null)
        qc.invalidateQueries({ queryKey: ['sessions'] })
      })
    }, 2_000)
    return () => {
      cancelled = true
      unlisten?.()
      clearInterval(interval)
    }
  }, [recording, qc, setActiveSessionId])

  // Render the toast through a portal to `document.body` rather than
  // inline in this subtree. Reason: when the user is on the History
  // tab, the wrapper `<div style={{ display: tab === 'live' ? 'flex' :
  // 'none' }}>` in App.tsx puts our entire subtree under
  // `display: none` — and CSS specifies that an element with
  // `display: none` doesn't generate a box, so **nothing** inside it
  // renders, including `position: fixed` descendants. A toast fired
  // by an EVENT_SESSION_ENDED arriving while History is visible
  // would silently vanish. The portal lives directly under <body>
  // and is unaffected.
  const toast =
    notice &&
    createPortal(
      <NoticeBanner notice={notice} onDismiss={() => setNotice(null)} />,
      document.body,
    )

  return (
    <Layout style={{ flex: 1, minWidth: 0 }}>
      {toast}
      <Sider
        width={sidebar.width}
        style={{
          borderRight: '1px solid var(--color-border-2)',
          background: 'var(--color-bg-2)',
          display: 'flex',
          flexDirection: 'column',
          position: 'relative',
        }}
      >
        <div
          style={{
            padding: '10px 10px 8px',
            borderBottom: '1px solid var(--color-border-2)',
            display: 'flex',
            flexDirection: 'column',
            gap: 6,
          }}
        >
          <DeviceSelector
            devices={data ?? []}
            selectedKey={selectedKey}
            onChange={(k) => {
              if (recording) {
                setNotice({
                  kind: 'info',
                  text: 'Stop the current session first.',
                  auto: true,
                })
                return
              }
              setSelectedKey(k)
            }}
            loading={isLoading}
            disabled={recording}
          />
          <AppSelector
            apps={appsQuery.data ?? []}
            value={targetPkg}
            onChange={setTargetPkg}
            loading={appsQuery.isLoading}
            disabled={recording || !selected}
          />
          <Space size="small" style={{ marginTop: 2 }}>
            {recording ? (
              <Button
                size="small"
                type="primary"
                status="danger"
                icon={<Square size={12} />}
                onClick={handleStop}
              >
                Stop
              </Button>
            ) : (
              <Button
                size="small"
                type="primary"
                icon={<Play size={12} />}
                onClick={handleStart}
                disabled={!selected || !selected.usable || !targetPkg}
                title={
                  !selected
                    ? 'Pick a device first.'
                    : !selected.usable
                      ? 'iOS via Wi-Fi only — connect over USB to enable sampling.'
                      : !targetPkg
                        ? 'Pick a target app first.'
                        : undefined
                }
              >
                Start
              </Button>
            )}
            <Button
              size="small"
              icon={<Bookmark size={12} />}
              onClick={handleAddMarker}
              title="Drop a marker on the timeline (Cmd+Shift+M)"
              disabled={!recording}
            >
              Marker{markers.length > 0 ? ` (${markers.length})` : ''}
            </Button>
          </Space>
        </div>
        <SidebarTabs selected={selected} />
        <div
          style={{
            padding: '6px 12px',
            borderTop: '1px solid var(--color-border-2)',
            fontSize: 10,
            color: 'var(--color-text-3)',
            letterSpacing: 0.02,
            textAlign: 'center',
          }}
        >
          mperf · 数据保存在本机
        </div>
        {/* Drag handle: a 4px wide strip flush to the right edge.
            Pointer events capture so the drag survives even when the
            cursor flies into the chart area. */}
        <div
          {...sidebar.handleProps}
          style={{
            position: 'absolute',
            top: 0,
            right: -2,
            width: 4,
            height: '100%',
            cursor: 'col-resize',
            zIndex: 10,
            background: sidebar.dragging
              ? 'var(--color-primary-light-3)'
              : 'transparent',
            transition: sidebar.dragging ? 'none' : 'background 120ms',
          }}
          onMouseEnter={(e) => {
            if (!sidebar.dragging) {
              ;(e.currentTarget as HTMLDivElement).style.background =
                'var(--color-fill-3)'
            }
          }}
          onMouseLeave={(e) => {
            if (!sidebar.dragging) {
              ;(e.currentTarget as HTMLDivElement).style.background = 'transparent'
            }
          }}
          title="拖动调整侧栏宽度"
        />
      </Sider>
      <Content
        style={{
          // Content now stacks: chart scroller (flex: 1) + log toolbar +
          // log terminal (optional). Without the flex column we couldn't
          // make the log panel anchor to the bottom edge of the chart
          // area while the chart list scrolls independently above it.
          display: 'flex',
          flexDirection: 'column',
          overflowX: 'hidden',
          minWidth: 0,
          minHeight: 0,
        }}
      >
        <div
          style={{
            flex: 1,
            overflowX: 'hidden',
            overflowY: 'auto',
            padding: 24,
            minWidth: 0,
            minHeight: 0,
          }}
        >
        {selected ? (
          <div style={{ minWidth: 0 }}>
            {/* NoticeBanner is rendered via createPortal above — see the
                comment near the `toast` definition. Keeping the toast
                in the LiveView subtree (here) is what caused
                EVENT_SESSION_ENDED toasts to silently vanish when the
                user was on the History tab. */}
            {!selected.usable && (
              <div
                style={{
                  padding: '10px 14px',
                  marginBottom: 12,
                  background: 'rgba(255, 125, 0, 0.10)',
                  border: '1px solid rgba(255, 125, 0, 0.40)',
                  borderRadius: 6,
                  color: 'rgb(255, 125, 0)',
                  fontSize: 13,
                  lineHeight: 1.5,
                }}
              >
                此 iOS 设备只通过 Wi-Fi 连接。采集需要 USB 上的
                CoreDeviceProxy 隧道（DTX / sysmontap 走的链路）—— 插上 USB
                后会自动可用。当前 iOS WiFi 入口主要为后续电量采集占位（电量必须 WiFi 测，避免 USB 充电干扰）。
              </div>
            )}
            <LiveCpuChart active={recording} markers={markerControls} />
            <LivePerCoreChart active={recording} markers={markerControls} />
            <LiveFpsChart
              active={recording}
              platform={selected.platform}
              markers={markerControls}
            />
            <LiveGpuChart active={recording} markers={markerControls} />
            {selected.platform === 'android' && (
              <LiveTemperatureChart active={recording} markers={markerControls} />
            )}
            {!targetPkg ? (
              <div className={chartStyles.chartCard}>
                <div className={chartStyles.chartHeader}>
                  <div className={chartStyles.chartTitle}>Memory</div>
                  <div className={chartStyles.chartSub}>App PSS · MB</div>
                </div>
                {/* Height tuned to match LiveMemoryChart's stats(75) + chartHost(240) so
                    the page doesn't jump when the user finally picks an app. */}
                <div
                  style={{
                    height: 315,
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                    background: 'var(--color-fill-1)',
                    border: '1px dashed var(--color-border-2)',
                    borderRadius: 8,
                    fontSize: 13,
                    color: 'var(--color-text-3)',
                    lineHeight: 1.6,
                    textAlign: 'center',
                    padding: '0 24px',
                  }}
                >
                  请先在上方选择目标 app —— CPU / FPS / 内存等都是按 app 维度采集的。
                </div>
              </div>
            ) : (
              <LiveMemoryChart
                active={recording}
                platform={selected.platform}
                markers={markerControls}
              />
            )}
          </div>
        ) : (
          <div>
            <Title heading={5} style={{ marginTop: 0 }}>
              Welcome
            </Title>
            <Text type="secondary">
              Connect an Android device over USB, authorize ADB, then pick it on the left.
            </Text>
          </div>
        )}
        </div>

        {/* Bottom toolbar — always present (a slim 28px strip), holds
            the 日志 checkbox + a hint about the log scope per platform.
            Sits below the chart scroller, above the (optional) log
            terminal panel. */}
        <div
          style={{
            flexShrink: 0,
            height: 28,
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            padding: '0 12px',
            borderTop: '1px solid var(--color-border-2)',
            background: 'var(--color-fill-1)',
            fontSize: 12,
            color: 'var(--color-text-2)',
          }}
        >
          {/* Device-wide-logs path turned out too noisy to be useful
              (Arco virtualized dropdowns choke once main thread is
              competing with ~2500 lines/s of unfiltered iOS os_trace).
              Hard-disable the checkbox until an app is picked — same
              gating shape as the Start button. We can revisit a
              device-wide opt-in path if there's a real use case, but
              the friendly hint-bar version was worse than blocking. */}
          <Tooltip
            content={
              !selected
                ? 'Pick a device first.'
                : !selected.usable
                  ? 'iOS via Wi-Fi only — connect over USB.'
                  : !targetPkg
                    ? 'Pick a target app first — device-wide logs are too noisy.'
                    : undefined
            }
            disabled={!!selected && !!selected.usable && !!targetPkg}
          >
            {/* Wrap in a span so the Tooltip still triggers when the
                Checkbox itself is `disabled` (Arco's disabled element
                doesn't fire mouseenter on its own). */}
            <span style={{ display: 'inline-flex' }}>
              <Checkbox
                checked={logOpen}
                onChange={(v) => setLogOpen(v)}
                disabled={!selected || !selected.usable || !targetPkg}
              >
                <span style={{ fontSize: 12 }}>日志</span>
              </Checkbox>
            </span>
          </Tooltip>
          {selected && logOpen && targetPkg && (
            <Text type="secondary" style={{ fontSize: 11 }}>
              {selected.platform === 'android'
                ? 'logcat · 已按 app filter'
                : 'os_trace · 已按 app filter（PID）'}
            </Text>
          )}
        </div>

        {/* Resize handle for the log panel — 4px tall, full width,
            cursor: row-resize. Same UX pattern as the sidebar's
            right-edge drag. */}
        {logOpen && selected && (
          <div
            onPointerDown={onLogHandlePointerDown}
            onPointerMove={onLogHandlePointerMove}
            onPointerUp={onLogHandlePointerUp}
            onPointerCancel={onLogHandlePointerUp}
            style={{
              flexShrink: 0,
              height: 4,
              cursor: 'row-resize',
              background: logDragging
                ? 'var(--color-primary-light-3)'
                : 'transparent',
              transition: logDragging ? 'none' : 'background 120ms',
            }}
            onMouseEnter={(e) => {
              if (!logDragging) {
                ;(e.currentTarget as HTMLDivElement).style.background =
                  'var(--color-fill-3)'
              }
            }}
            onMouseLeave={(e) => {
              if (!logDragging) {
                ;(e.currentTarget as HTMLDivElement).style.background = 'transparent'
              }
            }}
            title="拖动调整日志面板高度"
          />
        )}

        {logOpen && selected && (
          <LogTerminal
            enabled={logOpen}
            deviceId={selected.id}
            platform={selected.platform}
            targetPkg={targetPkg}
            heightPx={logHeight}
          />
        )}
      </Content>
    </Layout>
  )
}

const NOTICE_PALETTE = {
  info: { fg: 'rgb(22, 93, 255)', bg: 'rgba(22, 93, 255, 0.08)', border: 'rgba(22, 93, 255, 0.35)' },
  success: { fg: 'rgb(0, 180, 42)', bg: 'rgba(0, 180, 42, 0.08)', border: 'rgba(0, 180, 42, 0.35)' },
  warning: { fg: 'rgb(255, 125, 0)', bg: 'rgba(255, 125, 0, 0.10)', border: 'rgba(255, 125, 0, 0.40)' },
  error: { fg: 'rgb(245, 63, 63)', bg: 'rgba(245, 63, 63, 0.08)', border: 'rgba(245, 63, 63, 0.35)' },
} as const

function NoticeBanner({
  notice,
  onDismiss,
}: {
  notice: Notice
  onDismiss: () => void
}) {
  const p = NOTICE_PALETTE[notice.kind]
  // Top-center, just below the 48px Layout.Header. Why this position:
  //   - top-right (`top: 60 right: 24`) overlapped chart card header
  //     sub-text like "Total + App · %"
  //   - bottom-right (`bottom: 24 right: 24`) overlapped the bottom
  //     row of chart stat tiles or the Memory chart's lower content
  //   - top-center sits in the chart card's header padding gap
  //     between the left-aligned title and right-aligned subtitle, so
  //     it covers only whitespace; this is also the industry default
  //     (Ant Design, GitHub, Linear)
  // Fixed-position (instead of inline) so toast appear/dismiss doesn't
  // reflow chart content underneath. createPortal at the call site
  // also takes the toast out of LiveView's subtree so a `display: none`
  // tab toggle doesn't swallow it.
  return (
    <div
      style={{
        position: 'fixed',
        top: 60,
        left: '50%',
        transform: 'translateX(-50%)',
        zIndex: 100,
        padding: '10px 14px',
        background: p.bg,
        border: `1px solid ${p.border}`,
        borderRadius: 8,
        color: p.fg,
        fontSize: 13,
        display: 'flex',
        alignItems: 'center',
        gap: 12,
        maxWidth: 'min(560px, calc(100vw - 48px))',
        boxShadow: '0 4px 12px rgba(0, 0, 0, 0.08)',
      }}
    >
      <span>{notice.text}</span>
      <Button size="mini" onClick={onDismiss}>
        Dismiss
      </Button>
    </div>
  )
}
