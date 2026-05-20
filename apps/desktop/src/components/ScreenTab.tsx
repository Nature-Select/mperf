import { useEffect, useState } from 'react'
import { Button, Message, Tooltip } from '@arco-design/web-react'
import { Info, Play } from 'lucide-react'
import { Platform, measureStartup, type StartupMode } from '@/lib/ipc'
import styles from './ScreenTab.module.scss'

interface Props {
  /// Whether the ScreenShot metric is currently selected — controls
  /// the dark capture strip.
  screenshotOn: boolean
  /// Whether the Startup Timing metric is selected — controls the
  /// cold / hot startup measurement rows.
  startupTimingOn: boolean
  /// Active device + selected app — both needed to fire `measureStartup`.
  /// `null` for either disables the buttons with a tooltip.
  deviceId: string | null
  platform: Platform | null
  targetPkg: string | null
  /// Forwarded from LiveView's `startSession` response — the launch
  /// that just happened got auto-measured (mode detected from "is the
  /// app already running"). Each new value populates the matching
  /// row so the user doesn't need to click "测试" to see the data.
  autoStartup: { mode: StartupMode; total_ms: number } | null
}

/// Top strip above the chart list — mirrors PerfDog's "SceneTab".
///
/// Two halves, independently gated by metrics-picker selection:
/// 1. `ScreenShot` toggles the dark capture surface (PerfDog grabs a
///    still frame every ~2 s; backend not implemented yet, so this is
///    a placeholder).
/// 2. `Startup Timing` toggles a Cold / Hot start measurement row.
///    Each row has a "测试" button that force-restarts (cold) or
///    foregrounds (hot) the selected app and reports the total ms.
///    Values persist into `sessions.startup_timings` when a recording
///    session is active for the same app.
export function ScreenTab({
  screenshotOn,
  startupTimingOn,
  deviceId,
  platform,
  targetPkg,
  autoStartup,
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
        <StartupSection
          deviceId={deviceId}
          platform={platform}
          targetPkg={targetPkg}
          autoStartup={autoStartup}
        />
      )}
    </div>
  )
}

function StartupSection({
  deviceId,
  platform,
  targetPkg,
  autoStartup,
}: {
  deviceId: string | null
  platform: Platform | null
  targetPkg: string | null
  autoStartup: { mode: StartupMode; total_ms: number } | null
}) {
  const [coldMs, setColdMs] = useState<number | null>(null)
  const [hotMs, setHotMs] = useState<number | null>(null)
  const [busy, setBusy] = useState<StartupMode | null>(null)

  // Mirror each new auto-measurement (from start_session response)
  // into the matching row. Reference equality on `autoStartup` is
  // enough — LiveView creates a fresh object per Start.
  useEffect(() => {
    if (!autoStartup) return
    if (autoStartup.mode === 'cold') setColdMs(autoStartup.total_ms)
    else setHotMs(autoStartup.total_ms)
  }, [autoStartup])

  const canMeasure = !!deviceId && !!platform && !!targetPkg && busy === null
  const disabledReason = !deviceId
    ? '先选设备'
    : !targetPkg
      ? '先选目标 app'
      : busy
        ? `${busy === 'cold' ? '冷启动' : '热启动'}测试中…`
        : null

  const run = async (mode: StartupMode) => {
    if (!canMeasure || !deviceId || !platform || !targetPkg) return
    setBusy(mode)
    try {
      const res = await measureStartup(deviceId, platform, targetPkg, mode)
      if (mode === 'cold') setColdMs(res.total_ms)
      else setHotMs(res.total_ms)
      if (res.persisted_to_session) {
        Message.success({
          content: `${mode === 'cold' ? '冷' : '热'}启动 ${res.total_ms} ms · 已记入 session #${res.persisted_to_session}`,
          duration: 3000,
        })
      } else {
        Message.success({
          content: `${mode === 'cold' ? '冷' : '热'}启动 ${res.total_ms} ms`,
          duration: 2500,
        })
      }
    } catch (e) {
      Message.error({ content: String(e), duration: 5000 })
    } finally {
      setBusy(null)
    }
  }

  return (
    <div className={styles.startupSection}>
      <StartupRow
        label="冷启动"
        hint={
          'force-stop 后重启的总时长。' +
          'Android: am start -W TotalTime（kernel 测量,等到首帧渲染完成）。' +
          'iOS: processcontrol launchApp RPC 时长(进程创建到 PID 返回,不等 UIKit 初始化和首帧)—— ' +
          '所以 iOS 数字明显小于 Android,这是 Apple RPC 语义差异,不是测量误差。'
        }
        valueMs={coldMs}
        running={busy === 'cold'}
        disabled={!canMeasure}
        disabledReason={disabledReason ?? ''}
        onRun={() => run('cold')}
      />
      <StartupRow
        label="热启动"
        hint={
          'app 在后台时拉到前台测的时长,反映 UI 重建+恢复开销。' +
          'Android: am start -W TotalTime;若 app 已前台,自动按 HOME 退后台再测。' +
          'iOS: launchApp RPC 时长;若 app 已前台,先 launch SpringBoard 把它推到后台再测。' +
          '若 app 没在跑会退化为冷启动行为。'
        }
        valueMs={hotMs}
        running={busy === 'hot'}
        disabled={!canMeasure}
        disabledReason={disabledReason ?? ''}
        onRun={() => run('hot')}
      />
    </div>
  )
}

function StartupRow({
  label,
  hint,
  valueMs,
  running,
  disabled,
  disabledReason,
  onRun,
}: {
  label: string
  hint: string
  valueMs: number | null
  running: boolean
  disabled: boolean
  disabledReason: string
  onRun: () => void
}) {
  const valueText = valueMs == null ? '—' : `${valueMs} ms`
  const btn = (
    <Button
      size="mini"
      type="outline"
      icon={<Play size={11} />}
      onClick={onRun}
      disabled={disabled}
      loading={running}
    >
      测试
    </Button>
  )
  return (
    <div className={styles.startupRow}>
      <span className={styles.startupLabel}>{label}:</span>
      <span className={styles.startupValue}>{valueText}</span>
      <Tooltip content={hint} position="bottom">
        <Info size={11} className={styles.startupHint} />
      </Tooltip>
      <div className={styles.startupAction}>
        {disabled && disabledReason ? (
          <Tooltip content={disabledReason}>
            <span>{btn}</span>
          </Tooltip>
        ) : (
          btn
        )}
      </div>
    </div>
  )
}
