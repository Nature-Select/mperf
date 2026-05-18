import { pctColor, clampTooltip, formatClock } from '@/lib/format'
import styles from './chart-shared.module.scss'

/// A pinned tooltip rendered on top of a chart. Used for both single-value
/// (FPS) and multi-value (PerCore, CPU) variants. Lives in its own file
/// because every Live and Static chart needs it; previously it was an
/// export from LiveCpuChart.tsx, which made that file an accidental
/// shared module.
export function PinTooltip({
  left,
  top,
  t,
  single,
  rows,
}: {
  left: number
  top: number
  t: number
  single?: { label: string; value: number }
  rows?: Array<{ swatch: string; label: string; value: number }>
}) {
  return (
    <>
      <div className={styles.pinBar} style={{ left }} />
      <div
        className={`${styles.tooltip} ${styles.pinned}`}
        style={{ left: clampTooltip(left), top: top - 12 }}
      >
        <div className={styles.tooltipTime}>📌 {formatClock(t)}</div>
        {single && (
          <div className={styles.tooltipRow} style={{ color: pctColor(single.value) }}>
            {single.label}&nbsp;<b>{single.value.toFixed(1)}%</b>
          </div>
        )}
        {rows && (
          <div className={styles.tooltipGrid}>
            {rows.map((r, i) => (
              <div key={i} className={styles.tooltipRow}>
                <span className={styles.tooltipSwatch} style={{ background: r.swatch }} />
                {r.label}&nbsp;
                <b style={{ color: Number.isFinite(r.value) ? pctColor(r.value) : undefined }}>
                  {Number.isFinite(r.value) ? `${r.value.toFixed(1)}%` : '—'}
                </b>
              </div>
            ))}
          </div>
        )}
        <div className={styles.pinHint}>click chart or press Esc to unpin</div>
      </div>
    </>
  )
}
