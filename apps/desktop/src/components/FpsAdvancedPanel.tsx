import { FpsAdvancedStats } from '@/lib/fps-stats'
import { fpsColor } from '@/lib/format'
import styles from './FpsAdvancedPanel.module.scss'

interface Props {
  stats: FpsAdvancedStats | null
}

/// PerfDog-style detail panel: high-density grid of derived FPS stats,
/// rendered below the main chart. Always renders (with — placeholders
/// when stats is null) so the parent card's height is stable across
/// the no-data → first-data transition.
export function FpsAdvancedPanel({ stats }: Props) {
  const fmtPct = (v: number | undefined) =>
    v != null ? `${(v * 100).toFixed(1)}%` : '—'
  const fmt = (v: number | undefined, digits = 1) =>
    v != null ? v.toFixed(digits) : '—'
  const color = (v: number | undefined) => (v != null ? fpsColor(v) : undefined)

  return (
    <div className={styles.panel}>
      <Cell label="Median" value={fmt(stats?.median)} color={color(stats?.median)} />
      <Cell label="Std" value={fmt(stats?.std, 2)} />
      <Cell label="Var" value={fmt(stats?.variance, 2)} />
      <Cell label="1% Low" value={fmt(stats?.oneLow)} color={color(stats?.oneLow)} />
      <Cell
        label="MedRange"
        value={fmtPct(stats?.medRange)}
        hint="Share of frames within ±20% of median FPS"
        color={color(stats?.medRange != null ? stats.medRange * 100 : undefined)}
      />
      <Cell
        label="Drop / h"
        value={fmt(stats?.dropPerHour, 0)}
        hint="Count of >8fps drops, scaled to per-hour"
      />
      <Cell
        label="FPS ≥ 30"
        value={fmtPct(stats?.ge30)}
        color={color(stats?.ge30 != null ? stats.ge30 * 100 : undefined)}
      />
      <Cell
        label="FPS ≥ 45"
        value={fmtPct(stats?.ge45)}
        color={color(stats?.ge45 != null ? stats.ge45 * 100 : undefined)}
      />
      <Cell
        label="FPS ≥ 55"
        value={fmtPct(stats?.ge55)}
        color={color(stats?.ge55 != null ? stats.ge55 * 100 : undefined)}
      />
    </div>
  )
}

function Cell({
  label,
  value,
  hint,
  color,
}: {
  label: string
  value: string
  hint?: string
  color?: string
}) {
  return (
    <div className={styles.cell} title={hint}>
      <div className={styles.label}>
        {label}
        {hint && <span className={styles.hintMark}>ⓘ</span>}
      </div>
      <div className={styles.value} style={color ? { color } : undefined}>
        {value}
      </div>
    </div>
  )
}
