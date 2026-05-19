import { Tooltip } from '@arco-design/web-react'
import { Info } from 'lucide-react'
import styles from './ScreenTab.module.scss'

interface Props {
  /// Whether the ScreenShot metric is currently selected — controls
  /// the dark capture strip.
  screenshotOn: boolean
  /// Whether the Startup Timing metric is selected — controls the
  /// TTID / TTFD row.
  startupTimingOn: boolean
}

/// Top strip above the chart list — mirrors PerfDog's "SceneTab".
/// PerfDog grabs a still frame every ~2 seconds (not a live video
/// stream), and the TTID / TTFD row below it covers startup timing.
/// Both backends are still TODO, so the capture strip is an empty
/// "暂未实现" surface and the timing values render as `—`.
///
/// The two halves are independently gated by the user's metrics-picker
/// selection — selecting ScreenShot OR Startup Timing makes the
/// corresponding row appear; deselecting both hides the whole tab so
/// the chart list snaps back to the top.
export function ScreenTab({ screenshotOn, startupTimingOn }: Props) {
  if (!screenshotOn && !startupTimingOn) return null
  return (
    <div className={styles.wrap}>
      {screenshotOn && (
        <div className={styles.capture}>
          <div className={styles.capturePlaceholder}>
            屏幕截图（每 2s 一张）· 暂未实现
          </div>
        </div>
      )}
      {startupTimingOn && (
        <div className={styles.timingRow}>
          <TimingCell
            label="TTID(首屏时间)"
            value="—"
            hint="Time To Initial Display — 从启动到首帧渲染的耗时。后端尚未实现。"
          />
          <TimingCell
            label="TTFD(主屏时间)"
            value="—"
            hint="Time To Full Display — 从启动到首屏关键内容完全可交互的耗时。后端尚未实现。"
          />
        </div>
      )}
    </div>
  )
}

function TimingCell({
  label,
  value,
  hint,
}: {
  label: string
  value: string
  hint: string
}) {
  return (
    <div className={styles.timingCell}>
      <span className={styles.timingLabel}>{label}:</span>
      <span className={styles.timingValue}>{value}</span>
      <Tooltip content={hint} position="bottom">
        <Info size={11} className={styles.timingHint} />
      </Tooltip>
    </div>
  )
}
