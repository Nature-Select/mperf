import { useState } from 'react'
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
        <StartupSection deviceId={deviceId} platform={platform} targetPkg={targetPkg} />
      )}
    </div>
  )
}

function StartupSection({
  deviceId,
  platform,
  targetPkg,
}: {
  deviceId: string | null
  platform: Platform | null
  targetPkg: string | null
}) {
  const [coldMs, setColdMs] = useState<number | null>(null)
  const [hotMs, setHotMs] = useState<number | null>(null)
  const [busy, setBusy] = useState<StartupMode | null>(null)

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
        hint="force-stop 目标 app 后重新启动,测从冷启到首帧的总时长(Android: am start -W TotalTime;iOS: processcontrol 调用墙钟)"
        valueMs={coldMs}
        running={busy === 'cold'}
        disabled={!canMeasure}
        disabledReason={disabledReason ?? ''}
        onRun={() => run('cold')}
      />
      <StartupRow
        label="热启动"
        hint="app 在后台时拉到前台测的时长,反映 UI 重建+恢复开销;iOS 若 app 没在跑会退化为冷启动"
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
