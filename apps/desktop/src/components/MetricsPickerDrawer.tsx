import { useEffect, useMemo, useState } from 'react'
import { Checkbox, Drawer, Input, Select, Tag, Tooltip, Typography } from '@arco-design/web-react'
import { Plus, Search, X } from 'lucide-react'
import {
  CATEGORIES,
  DEFAULT_SELECTED_IDS,
  INTERVAL_OPTIONS_MS,
  METRICS,
  formatInterval,
  type MetricItem,
  categoryOf,
} from '@/lib/metricsCatalog'
import { useMetricsSelection } from '@/lib/useMetricsSelection'
import { useMetricFrequencies } from '@/lib/useMetricFrequencies'
import styles from './MetricsPickerDrawer.module.scss'

const { Text } = Typography

/// Floating "+" trigger anchored to the bottom-right of whatever
/// `position: relative` parent the caller mounts this in. Opens a right-
/// side Drawer with the full metric catalog (categorised, with disabled
/// rows for metrics not yet implemented in the backend).
///
/// Auto-save: every toggle / category bulk / 重置 writes straight to
/// localStorage via `useMetricsSelection`. Closing the drawer (X / mask
/// click / Esc) has no commit semantics — there's nothing to commit.
/// This matches PerfDog's behaviour and the modern settings-panel
/// pattern (macOS System Settings, iOS Settings, etc.).
///
/// `recording` locks the picker: during an active session the selection
/// is frozen for data-consistency reasons (a session whose first half
/// records FPS but second half doesn't is messy to plot and worse to
/// reason about post-hoc). If the user happens to have the drawer open
/// when they click Start, we auto-close it so the gate isn't ambiguous.
export function MetricsPickerDrawer({ recording = false }: { recording?: boolean }) {
  const [open, setOpen] = useState(false)
  useEffect(() => {
    if (recording) setOpen(false)
  }, [recording])
  // FAB markup is shared; the only thing that differs between idle and
  // recording is whether we wrap it in Arco's Tooltip. Recording state
  // skips the wrapper entirely (uses the native `title` attribute
  // instead) so any DOM artefacts Arco might surface — focus-trap
  // helpers, arrow elements, positional spans — can't leak through and
  // render as a second shape next to the disabled FAB.
  const fab = (
    <button
      type="button"
      aria-label="采集项"
      aria-disabled={recording}
      disabled={recording}
      title={recording ? '录制中无法修改采集项，停止后再调整' : undefined}
      className={`${styles.fab} ${recording ? styles.fabDisabled : ''}`}
      onClick={() => {
        if (recording) return
        setOpen(true)
      }}
    >
      <Plus size={20} strokeWidth={2.5} />
    </button>
  )
  return (
    <>
      {recording ? (
        fab
      ) : (
        <Tooltip content="选择采集项" position="left">
          {fab}
        </Tooltip>
      )}
      <Drawer
        title={null}
        visible={open}
        onCancel={() => setOpen(false)}
        footer={null}
        // We render our own close X inside the panel header so it
        // shares the flex row with 重置 / counter / title and lines up
        // cleanly. Arco's built-in X is absolutely positioned in the
        // drawer's own header strip and can't be co-aligned with our
        // body-level header.
        closable={false}
        width={400}
        bodyStyle={{ padding: 0 }}
      >
        <MetricsPickerPanel onClose={() => setOpen(false)} />
      </Drawer>
    </>
  )
}

function MetricsPickerPanel({ onClose }: { onClose: () => void }) {
  const { selected, toggle, setMany, commit } = useMetricsSelection()
  const { resolve: resolveInterval, set: setInterval } = useMetricFrequencies()
  const [query, setQuery] = useState('')

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (!q) return METRICS
    return METRICS.filter(
      (m) =>
        m.label.toLowerCase().includes(q) ||
        m.description.toLowerCase().includes(q) ||
        m.abbr.toLowerCase().includes(q),
    )
  }, [query])

  // Counter reflects every toggleable item — implemented samplers plus
  // previewable UI placeholders — since both contribute rows the user
  // can actually flip on.
  const toggleableTotal = METRICS.filter((m) => m.implemented || m.previewable).length
  const toggleableSelected = METRICS.filter(
    (m) => (m.implemented || m.previewable) && selected.has(m.id),
  ).length

  const resetDefaults = () => {
    commit(new Set(DEFAULT_SELECTED_IDS))
  }

  const grouped = useMemo(() => {
    const byCat = new Map<string, MetricItem[]>()
    for (const m of filtered) {
      const arr = byCat.get(m.category) ?? []
      arr.push(m)
      byCat.set(m.category, arr)
    }
    return CATEGORIES.map((c) => ({
      cat: c,
      items: byCat.get(c.id) ?? [],
    })).filter((g) => g.items.length > 0)
  }, [filtered])

  return (
    <div className={styles.panel}>
      <div className={styles.header}>
        <div className={styles.headerTop}>
          <Text bold style={{ fontSize: 15 }}>
            采集项
          </Text>
          <Text type="secondary" style={{ fontSize: 11 }}>
            已选 {toggleableSelected} / {toggleableTotal}
          </Text>
          <button
            type="button"
            className={styles.headerReset}
            onClick={resetDefaults}
            title="恢复默认选择"
          >
            重置
          </button>
          <button
            type="button"
            className={styles.headerClose}
            onClick={onClose}
            aria-label="关闭"
            title="关闭"
          >
            <X size={14} />
          </button>
        </div>
        <Input
          allowClear
          value={query}
          onChange={setQuery}
          placeholder="搜索采集项…"
          prefix={<Search size={12} />}
          size="small"
          style={{ marginTop: 8 }}
        />
      </div>

      <div className={styles.body}>
        {grouped.length === 0 ? (
          <div className={styles.empty}>没有匹配的采集项</div>
        ) : (
          grouped.map(({ cat, items }) => {
            const catToggleable = items.filter((i) => i.implemented || i.previewable)
            const catSelected = catToggleable.filter((i) => selected.has(i.id)).length
            const allOn =
              catToggleable.length > 0 && catSelected === catToggleable.length
            return (
              <section key={cat.id} className={styles.section}>
                <div className={styles.sectionHeader}>
                  <span
                    className={styles.sectionBar}
                    style={{ background: cat.color }}
                  />
                  <span className={styles.sectionTitle}>{cat.label}</span>
                  <span className={styles.sectionCount}>
                    {catSelected} / {catToggleable.length}
                  </span>
                  {catToggleable.length > 0 && (
                    <button
                      type="button"
                      className={styles.sectionToggle}
                      onClick={() =>
                        setMany(
                          catToggleable.map((i) => i.id),
                          !allOn,
                        )
                      }
                    >
                      {allOn ? '全不选' : '全选'}
                    </button>
                  )}
                </div>
                <div className={styles.items}>
                  {items.map((m) => (
                    <MetricRow
                      key={m.id}
                      item={m}
                      checked={selected.has(m.id)}
                      onToggle={() => toggle(m.id)}
                      intervalMs={resolveInterval(m.id)}
                      onIntervalChange={(ms) => setInterval(m.id, ms)}
                    />
                  ))}
                </div>
              </section>
            )
          })
        )}
      </div>
    </div>
  )
}

function MetricRow({
  item,
  checked,
  onToggle,
  intervalMs,
  onIntervalChange,
}: {
  item: MetricItem
  checked: boolean
  onToggle: () => void
  /// Effective interval (catalog default or user override). `undefined`
  /// for non-chart-backed items (capture placeholders) — no dropdown
  /// rendered in that case.
  intervalMs: number | undefined
  onIntervalChange: (ms: number) => void
}) {
  const cat = categoryOf(item.category)
  // `previewable` items can be toggled even when the sampler isn't
  // ready — they drive UI-only placeholders (e.g. ScreenTab) so users
  // see what's coming. The "未实现" tag still renders below to keep
  // the honest signal.
  const disabled = !item.implemented && !item.previewable
  const rowClick = () => {
    if (disabled) return
    onToggle()
  }
  // Frequency dropdown only for chart-backed metrics — capture
  // placeholders and "no-sampler" rows don't have a cadence to tune.
  // Hidden until the metric is checked so users don't fiddle with
  // rates that aren't actively recording.
  const showInterval = intervalMs != null && checked && item.implemented
  return (
    <div
      role="checkbox"
      aria-checked={checked}
      aria-disabled={disabled}
      tabIndex={disabled ? -1 : 0}
      className={`${styles.row} ${disabled ? styles.rowDisabled : ''}`}
      onClick={rowClick}
      onKeyDown={(e) => {
        if (disabled) return
        // Only react when the row's wrapping div itself is focused —
        // when focus is on the inner Checkbox, its native keydown
        // already toggles via onChange, and our outer handler would
        // double-toggle if we didn't short-circuit here.
        if (e.target !== e.currentTarget) return
        if (e.key === ' ' || e.key === 'Enter') {
          e.preventDefault()
          onToggle()
        }
      }}
    >
      <span
        className={`${styles.avatar} ${disabled ? styles.avatarMuted : ''}`}
        style={disabled ? undefined : { background: cat.color }}
      >
        {item.abbr}
      </span>
      <div className={styles.rowText}>
        <div className={styles.rowLine}>
          <span className={styles.rowLabel}>{item.label}</span>
          {item.platforms.length === 1 && (
            <Tag size="small" color="arcoblue" bordered className={styles.platformTag}>
              {item.platforms[0] === 'android' ? 'Android' : 'iOS'} only
            </Tag>
          )}
          {disabled && (
            <Tag size="small" color="gray" bordered className={styles.platformTag}>
              未实现
            </Tag>
          )}
        </div>
        <div className={styles.rowDesc}>{item.description}</div>
      </div>
      <div className={styles.rowTrailing} onClick={(e) => e.stopPropagation()}>
        {showInterval && (
          <Select
            size="mini"
            value={intervalMs}
            onChange={(v) => onIntervalChange(Number(v))}
            triggerProps={{ autoAlignPopupWidth: false }}
            className={styles.intervalSelect}
          >
            {INTERVAL_OPTIONS_MS.map((ms) => (
              <Select.Option key={ms} value={ms}>
                {formatInterval(ms)}
              </Select.Option>
            ))}
          </Select>
        )}
        <Checkbox
          checked={checked}
          disabled={disabled}
          onChange={() => onToggle()}
        />
      </div>
    </div>
  )
}
