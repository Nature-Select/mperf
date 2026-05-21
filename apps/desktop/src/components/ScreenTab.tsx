import { Tooltip } from '@arco-design/web-react'
import { Info } from 'lucide-react'
import { type StartupMode } from '@/lib/ipc'
import styles from './ScreenTab.module.scss'

interface Props {
  /// Whether the ScreenShot metric is currently selected — controls
  /// the dark capture strip.
  screenshotOn: boolean
  /// Whether the Startup Timing metric is selected — controls the
  /// single startup-timing readout row.
  startupTimingOn: boolean
  /// Forwarded from LiveView's `startSession` response — the launch
  /// that just happened got auto-measured (mode detected from "is
  /// the app already running"). Mirrors into the row so the user can
  /// see the value without doing anything extra.
  autoStartup: { mode: StartupMode; total_ms: number } | null
  /// Seconds remaining on the iOS kperf cooldown after a cold-startup
  /// measurement. 0 when not in cooldown. Shown next to the value as
  /// "下次冷启动测量可用 · 还需 XXs" so the user can plan re-records
  /// without guessing at the 30s wait.
  cooldownRemainingSec: number
}

/// Top strip above the chart list — mirrors PerfDog's "SceneTab".
///
/// Two halves, independently gated by metrics-picker selection:
/// 1. `ScreenShot` toggles the dark capture surface.
/// 2. `Startup Timing` toggles a single readout row.
///    Measurement fires automatically at session start: backend
///    detects whether the target app is already running (→ hot) or
///    needs a launch (→ cold) and measures once. No manual button —
///    iOS only supports one kperf consumer at a time and releases
///    the lock asynchronously after the previous session exits, so
///    multi-shot measurement on demand isn't feasible.
export function ScreenTab({
  screenshotOn,
  startupTimingOn,
  autoStartup,
  cooldownRemainingSec,
}: Props) {
  if (!screenshotOn && !startupTimingOn) return null
  return (
    <div className={styles.wrap}>
      {screenshotOn && (
        <div className={styles.capture}>
          <div className={styles.capturePlaceholder}>
            屏幕截图(每 2s 一张) · 暂未实现
          </div>
        </div>
      )}
      {startupTimingOn && (
        <StartupReadout
          autoStartup={autoStartup}
          cooldownRemainingSec={cooldownRemainingSec}
        />
      )}
    </div>
  )
}

function StartupReadout({
  autoStartup,
  cooldownRemainingSec,
}: {
  autoStartup: { mode: StartupMode; total_ms: number } | null
  cooldownRemainingSec: number
}) {
  // Pure display — render straight from the prop. Previous version
  // mirrored into local state, which got stuck on the first-recording
  // value after the user did a hot start then a cold start in the
  // same session window (the second start's autoStartup reference
  // updated the prop but the local-state useEffect didn't refresh).
  const valueText = autoStartup == null ? '—' : `${autoStartup.total_ms} ms`
  const typeText =
    autoStartup == null
      ? '等待录制'
      : autoStartup.mode === 'cold'
        ? '冷启动'
        : '热启动'
  const hint =
    '点击「录制」时自动测量。' +
    '若 app 未在跑(后台或被杀)记为冷启动,在跑(前台或后台)记为热启动。' +
    'Android 通过 am start -W TotalTime(kernel 测量到首帧);' +
    'iOS 冷启动通过 coreprofilesessiontap 的 kdebug 事件流估算首帧,热启动取 processcontrol launchApp RPC 时长。' +
    'iOS 内核 kperf 锁释放是异步的,两次冷启动测量请间隔 30s 以上,否则会失败。' +
    '冷却中再次开始录制会弹出确认窗口,可选择继续等待或跳过启动时间测量。'

  return (
    <div className={styles.startupSection}>
      <div className={styles.startupRow}>
        <span className={styles.startupLabel}>启动时间:</span>
        <span className={styles.startupValue}>{valueText}</span>
        <span className={styles.startupType}>· {typeText}</span>
        {cooldownRemainingSec > 0 && (
          <span className={styles.startupCooldown}>
            · 冷启动冷却中,还需 {cooldownRemainingSec}s
          </span>
        )}
        <Tooltip content={hint} position="bottom">
          <Info size={11} className={styles.startupHint} />
        </Tooltip>
      </div>
    </div>
  )
}
