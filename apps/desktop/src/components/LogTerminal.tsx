import { useEffect, useMemo, useRef, useState } from 'react'
import { listen, UnlistenFn } from '@tauri-apps/api/event'
import { Button, Input, Select, Tooltip } from '@arco-design/web-react'
import { Pause, Play, Trash2 } from 'lucide-react'
import {
  EVENT_LOG_LINE,
  EVENT_LOG_STATUS,
  LogLevel,
  LogLine,
  LogStreamStatus,
  Platform,
  startLogStream,
  stopLogStream,
} from '@/lib/ipc'
import styles from './LogTerminal.module.scss'

const { Option } = Select

/// Ring-buffer cap. ~5000 lines × ~120 char average ≈ 600KB JS heap.
/// Why 5000 not 1000: on iOS with the "all device" unfiltered path
/// (user opens the log terminal before picking a target app),
/// throughput hits ~2500 lines/s. With a 1000-line buffer the user's
/// search keyword would land in the buffer, hit a few rows, then those
/// rows get evicted within 0.4s — the result list visibly flickers
/// and feels like "search doesn't work". 5000 buys ~2s at full
/// unfiltered throughput, plenty for search to feel stable. Going
/// much higher slows the per-render filter loop (~O(N) string checks)
/// without much benefit. The right answer when this 2s window isn't
/// enough is "select an app first so the backend filters by PID and
/// throughput drops 1-2 orders of magnitude" — the toolbar hint
/// reminds users.
const RING_BUFFER_SIZE = 5000

/// Coalesce incoming log events into batches before triggering React
/// re-renders. Without batching, each EVENT_LOG_LINE callback calls
/// setVersion → React re-renders → useMemo re-filters the whole
/// buffer. At 2500 lines/s that's 2500 re-renders/s, which kills FPS
/// and also makes the search input "lag" visibly. 100ms batches mean
/// at most 10 renders/s under any throughput, with each render
/// covering up to ~250 new lines. Search responsiveness is set by
/// the search box's onChange (which is already React-controlled) +
/// the next batch flush — still under 100ms perceived latency.
const BATCH_FLUSH_INTERVAL_MS = 100

const LEVEL_ORDER: LogLevel[] = [
  'verbose',
  'debug',
  'info',
  'warn',
  'error',
  'fatal',
]

interface Props {
  /// Drive lifecycle from the parent — `LogTerminal` itself doesn't
  /// know when the user wants the stream off, it just consumes
  /// events. Parent should pass `enabled=false` to stop the stream
  /// and clear the buffer.
  ///
  /// The parent (LiveView) hard-gates this toggle on `targetPkg !=
  /// null`, so `targetPkg` will always be non-null when `enabled` is
  /// true. We don't try to support device-wide logs anymore — the
  /// noise level (especially iOS os_trace at ~2500 lines/s) made the
  /// UI unusable when combined with the user picking an app to filter.
  enabled: boolean
  deviceId: string | null
  platform: Platform | null
  /// Android passes this to logcat as a `--pid` filter (resolved
  /// server-side via `pidof <pkg>`). iOS uses it to resolve bundle id
  /// → exec → PID for client-side filtering.
  targetPkg: string | null
  /// Height in px. Owned by the parent so the resize handle (on the
  /// LiveView side of the boundary) can adjust it.
  heightPx: number
}

export function LogTerminal({
  enabled,
  deviceId,
  platform,
  targetPkg,
  heightPx,
}: Props) {
  /// Ring buffer — newest at the end. Stored in a ref because every
  /// new line otherwise triggers a re-render of the whole virtualized
  /// list, which costs more than just nudging a "buffer version" counter.
  const linesRef = useRef<LogLine[]>([])
  const [version, setVersion] = useState(0)
  const [paused, setPaused] = useState(false)
  const pausedRef = useRef(false)
  const [minLevel, setMinLevel] = useState<LogLevel>('verbose')
  const [search, setSearch] = useState('')
  /// Auto-scroll-to-bottom unless the user has scrolled up to read.
  /// Tracked by "is the scroll position near the bottom on the last
  /// scroll event"; when it is, every new batch scrollToBottom()s.
  const [autoScroll, setAutoScroll] = useState(true)
  const autoScrollRef = useRef(true)
  const scrollerRef = useRef<HTMLDivElement>(null)
  /// Stream-switch fence: every time we tear down and restart the
  /// backend stream (device change, app change, toggle on/off), we
  /// stamp `Date.now()` here. The listen handler drops any line whose
  /// `ts_ms` is older than this fence so in-flight events from a
  /// just-stopped stream (queued in the Tauri event channel before
  /// the backend actually exited) can't bleed into the new device's
  /// buffer. Backend stamps `ts_ms` from the host monotonic clock at
  /// emit time, so cross-comparison is sound.
  const streamResetMsRef = useRef(0)
  /// Backend attach state. null until the first status event arrives.
  /// Reset on every stream lifecycle re-run so a stale "attached" from
  /// a previous device can't survive a switch.
  const [status, setStatus] = useState<LogStreamStatus | null>(null)

  useEffect(() => {
    pausedRef.current = paused
  }, [paused])

  useEffect(() => {
    autoScrollRef.current = autoScroll
  }, [autoScroll])

  /// Lifecycle: when the toggle turns on, start the backend stream.
  /// When it turns off (or device/pkg/platform changes), stop and
  /// clear. `enabled` is the master switch; the underlying stream
  /// matches it.
  useEffect(() => {
    if (!enabled || !deviceId || !platform) return
    let cancelled = false
    void startLogStream(deviceId, platform, targetPkg).catch((e) => {
      // Component already cleaned up (user toggled off or switched
      // device while the start IPC was in-flight) — don't spam the
      // console for an error the user can't see anymore.
      if (cancelled) return
      console.error('[mperf] start_log_stream failed', e)
    })
    return () => {
      cancelled = true
      // Fence first, then clear the buffer. The fence prevents
      // in-flight events emitted by the backend stream between
      // "stop request sent" and "stream task actually exited" from
      // sneaking into the new stream's buffer. Bumping `version`
      // forces a re-render so the now-empty buffer is reflected.
      streamResetMsRef.current = Date.now()
      linesRef.current = []
      setStatus(null)
      setVersion((v) => v + 1)
      void stopLogStream().catch((e) =>
        console.error('[mperf] stop_log_stream failed', e),
      )
    }
  }, [enabled, deviceId, platform, targetPkg])

  /// Subscribe to backend attach-state transitions. Same fence as log
  /// lines so a stale `Attached{pid}` from a torn-down stream can't
  /// overwrite the new stream's `Waiting`.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined
    let cancelled = false
    listen<LogStreamStatus>(EVENT_LOG_STATUS, (e) => {
      if (e.payload.ts_ms < streamResetMsRef.current) return
      setStatus(e.payload)
    }).then((fn) => {
      if (cancelled) fn()
      else unlisten = fn
    })
    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])

  /// Listen for log lines from the backend. Mount once per
  /// component lifetime — same pattern as the chart Sample listeners.
  ///
  /// Hot path: the listener does the cheap thing only (push to a
  /// pending JS array). A separate 100ms timer flushes pending →
  /// ring buffer and bumps `version` to trigger one React render
  /// per batch. See `BATCH_FLUSH_INTERVAL_MS` for the why.
  useEffect(() => {
    const pending: LogLine[] = []
    let unlisten: UnlistenFn | undefined
    let cancelled = false
    listen<LogLine>(EVENT_LOG_LINE, (e) => {
      if (pausedRef.current) return
      // Drop in-flight lines from a stream we just tore down — see
      // `streamResetMsRef` comment above.
      if (e.payload.ts_ms < streamResetMsRef.current) return
      pending.push(e.payload)
    }).then((fn) => {
      if (cancelled) fn()
      else unlisten = fn
    })
    const flushTimer = setInterval(() => {
      if (pending.length === 0) return
      const buf = linesRef.current
      // `push(...pending)` spreads up to ~thousands of elements onto
      // the stack — V8 caps spread arg count, so do a plain loop
      // for safety at the kind of bursts we see (~2500 lines/s during
      // unfiltered iOS streams).
      for (let i = 0; i < pending.length; i++) buf.push(pending[i])
      pending.length = 0
      // Bulk-trim instead of shift-per-push (shift is O(n) on arrays).
      // +200 slack so we trim in chunks rather than every batch.
      if (buf.length > RING_BUFFER_SIZE + 200) {
        buf.splice(0, buf.length - RING_BUFFER_SIZE)
      }
      setVersion((v) => v + 1)
    }, BATCH_FLUSH_INTERVAL_MS)
    return () => {
      cancelled = true
      unlisten?.()
      clearInterval(flushTimer)
    }
  }, [])

  /// Filtered view of the buffer, recomputed on buffer version /
  /// filter changes. Cap at the ring size since we don't trim
  /// aggressively inside the push hot path.
  const filtered = useMemo(() => {
    const buf = linesRef.current
    const minIdx = LEVEL_ORDER.indexOf(minLevel)
    const q = search.trim().toLowerCase()
    const out: LogLine[] = []
    for (const ln of buf) {
      const lvlIdx = LEVEL_ORDER.indexOf(ln.level)
      if (lvlIdx !== -1 && lvlIdx < minIdx) continue
      if (q) {
        const hay = `${ln.tag} ${ln.message} ${ln.process ?? ''}`.toLowerCase()
        if (!hay.includes(q)) continue
      }
      out.push(ln)
    }
    return out.slice(-RING_BUFFER_SIZE)
    // `version` is the buffer-mutation signal; intentionally included.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version, minLevel, search])

  /// Auto-scroll after every render. Cheap because scrollTop set
  /// from JS doesn't re-trigger React.
  useEffect(() => {
    if (!autoScrollRef.current || !scrollerRef.current) return
    const el = scrollerRef.current
    el.scrollTop = el.scrollHeight
  }, [filtered])

  const onScroll = (e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget
    // 24px slack to feel forgiving — user scrolled "near" the bottom
    // counts as "still tailing".
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24
    if (atBottom !== autoScrollRef.current) {
      setAutoScroll(atBottom)
    }
  }

  const clear = () => {
    linesRef.current = []
    setVersion((v) => v + 1)
  }

  return (
    <div className={styles.terminal} style={{ height: heightPx }}>
      <div className={styles.toolbar}>
        <Tooltip content={paused ? '继续接收' : '暂停接收（buffer 保留）'}>
          <Button
            size="mini"
            type="text"
            icon={paused ? <Play size={12} /> : <Pause size={12} />}
            onClick={() => setPaused((p) => !p)}
          >
            {paused ? '已暂停' : '运行中'}
          </Button>
        </Tooltip>
        <Tooltip content="清空 buffer">
          <Button
            size="mini"
            type="text"
            icon={<Trash2 size={12} />}
            onClick={clear}
          />
        </Tooltip>
        <Select
          size="mini"
          value={minLevel}
          onChange={(v) => setMinLevel(v as LogLevel)}
          // 108 + the popup auto-aligned to the trigger fits the
          // longest label ("≥ Verbose") without ellipsis in Arco's
          // mini size (font 12px, internal padding+arrow ~32px).
          style={{ width: 108 }}
        >
          <Option value="verbose">≥ Verbose</Option>
          <Option value="debug">≥ Debug</Option>
          <Option value="info">≥ Info</Option>
          <Option value="warn">≥ Warn</Option>
          <Option value="error">≥ Error</Option>
          {/* "≥ Fatal" (was "Fatal only") for visual symmetry with
              the other options — Fatal is the highest level, so
              "≥ Fatal" is semantically identical to "Fatal only". */}
          <Option value="fatal">≥ Fatal</Option>
        </Select>
        <Input
          size="mini"
          placeholder="过滤 tag / 内容 / process"
          value={search}
          onChange={setSearch}
          style={{ width: 220 }}
          allowClear
        />
        <StatusBadge status={status} targetPkg={targetPkg} />
        <span className={styles.counter}>
          {filtered.length}/{linesRef.current.length}
          {paused && ' · paused'}
          {!autoScroll && ' · 已停滚'}
        </span>
      </div>
      <div ref={scrollerRef} className={styles.scroller} onScroll={onScroll}>
        {filtered.map((ln, i) => (
          <LogLineRow key={i} line={ln} />
        ))}
      </div>
    </div>
  )
}

/// Attach-state badge in the toolbar. Three states for the user:
///   - Waiting   → 等待 <pkg> 启动…  (yellow, with the auto-launch hint)
///   - Attached  → ● PID 12345       (green)
///   - Unknown   → nothing rendered  (pre-status / iOS path before
///                                    we emit the first event)
function StatusBadge({
  status,
  targetPkg,
}: {
  status: LogStreamStatus | null
  targetPkg: string | null
}) {
  if (!status) return null
  if (status.state === 'waiting') {
    return (
      <Tooltip content="目标 app 未运行。终端会在它启动后自动 attach；如果应用重启 PID 变化也会自动续接。">
        <span className={`${styles.badge} ${styles.badge_waiting}`}>
          ● 等待 {shortenPkg(targetPkg)} 启动…
        </span>
      </Tooltip>
    )
  }
  return (
    <Tooltip content="已 attach；应用重启 PID 变化会自动续接。">
      <span className={`${styles.badge} ${styles.badge_attached}`}>
        ● PID {status.pid}
      </span>
    </Tooltip>
  )
}

function shortenPkg(pkg: string | null): string {
  if (!pkg) return 'app'
  // For Android-style dotted package names, last segment is usually
  // enough context — "com.example.foo.MainApp" → "MainApp". Keep iOS
  // bundle ids whole; they're often shorter and the segments matter.
  if (pkg.includes('.') && pkg.split('.').length >= 3) {
    return pkg.split('.').pop() || pkg
  }
  return pkg
}

function LogLineRow({ line }: { line: LogLine }) {
  // Unified column layout for both platforms:
  //   [level] [process] [pid] [tag/subsystem][·subcategory] [message]
  // Platform-specific gaps:
  //   - Android: process absent → empty slot keeps column alignment
  //   - iOS without os_log label: tag empty → empty slot
  // All slots are fixed-width (CSS) so rows line up regardless of
  // which fields are filled. An empty slot renders as "—" so the
  // user can tell "this row didn't have a process name" vs "the
  // column doesn't exist".
  const tagText =
    line.subcategory && line.tag
      ? `${line.tag}·${line.subcategory}`
      : line.subcategory || line.tag
  return (
    <div className={`${styles.row} ${styles[`level_${line.level}`] ?? ''}`}>
      <span className={styles.level}>{LEVEL_GLYPH[line.level]}</span>
      <span className={styles.process}>{line.process ?? ''}</span>
      <span className={styles.pid}>{line.pid != null ? line.pid : ''}</span>
      <span className={styles.tag}>{tagText}</span>
      <span className={styles.msg}>{line.message}</span>
    </div>
  )
}

const LEVEL_GLYPH: Record<LogLevel, string> = {
  verbose: 'V',
  debug: 'D',
  info: 'I',
  warn: 'W',
  error: 'E',
  fatal: 'F',
  unknown: '?',
}
