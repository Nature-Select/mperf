import { useEffect, useRef, useState } from 'react'
import uPlot from 'uplot'
import { Marker } from '@/lib/ipc'
import { MarkerControls } from '@/lib/markers'

const STRIP_HALF_WIDTH_PX = 4 // 8px hit-area centered on the dashed line
const LINE_COLOR = 'rgba(100, 116, 139, 0.55)' // slate gray
/// Mouse-movement threshold (CSS px). Below this we treat mouseup as a
/// click (open popover); above as a drag (commit position).
const CLICK_DRAG_THRESHOLD_PX = 3

type DragState = {
  id: number
  startClientX: number
  /// True once we've crossed the click→drag threshold.
  movedPastThreshold: boolean
}

type EditState = {
  markerId: number
  draftLabel: string
}

/// Renders draggable HTML strips over a uPlot chart, one per marker.
///
/// Why HTML over canvas: canvas hit-testing for thin vertical lines is
/// painful, and uPlot's draw hook doesn't compose well with cursor
/// state. Each strip is an 8-px-wide invisible grab area with a thin
/// dashed visual line at its center; mousedown starts a potential
/// drag, global listeners track movement and commit on mouseup. If the
/// pointer didn't move much, we treat it as a click and open a label
/// editor popover (Grafana style — text edits get explicit Save/Cancel
/// because typos are easier than dragging the wrong way).
///
/// Drag state for *position* is not owned here — every chart in a
/// session shows the same markers, so the dragged position must
/// propagate to all of them. The parent owns dragMarker, computes the
/// override, passes the resulting `markers` array down. Callbacks
/// here just report drag-move / drag-end / delete / label-edit up.
///
/// The label-edit popover IS owned here (single chart at a time can
/// be in edit mode; we pin which marker is being edited locally).
export function MarkerOverlay({
  plot,
  wallStartSec,
  controls,
}: {
  plot: uPlot | null
  wallStartSec: number
  /// All marker state + handlers in one bundle. Optional so a chart can
  /// be rendered marker-less (e.g. a future readonly mode).
  controls?: MarkerControls
}) {
  const markers = controls?.list ?? []
  const onDragMove = controls?.onDragMove
  const onDragEnd = controls?.onDragEnd
  const onDelete = controls?.onDelete
  const onLabelEdit = controls?.onLabelEdit
  const dragStateRef = useRef<DragState | null>(null)
  const plotRefRef = useRef<uPlot | null>(plot)
  plotRefRef.current = plot
  const wallStartRef = useRef<number>(wallStartSec)
  wallStartRef.current = wallStartSec
  const onDragMoveRef = useRef(onDragMove)
  onDragMoveRef.current = onDragMove
  const onDragEndRef = useRef(onDragEnd)
  onDragEndRef.current = onDragEnd
  /// The drag/click handlers below are attached once with empty-deps so
  /// mid-drag state isn't reset on every markers change. We mirror
  /// `markers` into a ref so the click-to-edit path can look up the
  /// just-added marker by id (without this, the closure sees the empty
  /// array from initial render and the popover never opens for any
  /// marker created during the session).
  const markersRef = useRef<Marker[]>(markers)
  markersRef.current = markers

  /// Which marker (if any) currently has its popover open.
  const [edit, setEdit] = useState<EditState | null>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  // Auto-focus the input when the popover opens.
  useEffect(() => {
    if (edit) {
      // setTimeout to wait for the popover to mount and animate in.
      const id = setTimeout(() => inputRef.current?.focus(), 0)
      return () => clearTimeout(id)
    }
  }, [edit?.markerId])

  // Global drag listeners — attached lazily on mousedown, removed on
  // mouseup so we don't leak.
  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      const ds = dragStateRef.current
      const u = plotRefRef.current
      if (!ds || !u) return
      const movement = Math.abs(e.clientX - ds.startClientX)
      if (!ds.movedPastThreshold && movement < CLICK_DRAG_THRESHOLD_PX) {
        // Still might be a click; don't fire onDragMove.
        return
      }
      ds.movedPastThreshold = true
      const rect = u.over.getBoundingClientRect()
      const cssX = e.clientX - rect.left
      const t = u.posToVal(cssX, 'x')
      const tsUs = Math.max(0, Math.round((t - wallStartRef.current) * 1_000_000))
      onDragMoveRef.current?.(ds.id, tsUs)
    }
    const onUp = (e: MouseEvent) => {
      const ds = dragStateRef.current
      if (!ds) return
      dragStateRef.current = null
      document.body.style.cursor = ''
      const u = plotRefRef.current
      if (!u) return
      if (ds.movedPastThreshold) {
        // Real drag — commit final position.
        const rect = u.over.getBoundingClientRect()
        const cssX = e.clientX - rect.left
        const t = u.posToVal(cssX, 'x')
        const tsUs = Math.max(0, Math.round((t - wallStartRef.current) * 1_000_000))
        onDragEndRef.current?.(ds.id, tsUs)
      } else {
        // Treated as click → open label editor. Read via ref because
        // `markers` from closure is the empty initial-render array.
        const m = markersRef.current.find((x) => x.id === ds.id)
        if (m) setEdit({ markerId: m.id, draftLabel: m.label ?? '' })
      }
    }
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)
    return () => {
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup', onUp)
    }
    // Handlers read `markersRef.current` so the listener never needs
    // to be re-attached on markers change. Re-attaching would lose
    // mid-drag state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  if (!plot || plot.bbox == null || markers.length === 0) {
    return null
  }
  const over = plot.over
  const overRect = over.getBoundingClientRect()
  const hostRect = (plot.root as HTMLElement).getBoundingClientRect()
  const offsetLeft = overRect.left - hostRect.left
  const offsetTop = overRect.top - hostRect.top
  const drawWidth = overRect.width
  const drawHeight = overRect.height

  const editingMarker =
    edit != null ? markers.find((m) => m.id === edit.markerId) ?? null : null
  const editingX =
    editingMarker != null
      ? plot.valToPos(
          wallStartRef.current + editingMarker.ts_us / 1_000_000,
          'x',
          false,
        )
      : 0

  return (
    <>
      {markers.map((m) => {
        const t = wallStartRef.current + m.ts_us / 1_000_000
        const x = plot.valToPos(t, 'x', false)
        if (x < -STRIP_HALF_WIDTH_PX || x > drawWidth + STRIP_HALF_WIDTH_PX) {
          return null
        }
        return (
          <div
            key={m.id}
            onMouseDown={(e) => {
              if (e.button !== 0) return
              e.preventDefault()
              dragStateRef.current = {
                id: m.id,
                startClientX: e.clientX,
                movedPastThreshold: false,
              }
              document.body.style.cursor = 'ew-resize'
            }}
            title={
              m.label
                ? `${m.label} · ${formatOffset(m.ts_us)} (drag to move, click to edit)`
                : `Marker · ${formatOffset(m.ts_us)} (drag to move, click to edit)`
            }
            style={{
              position: 'absolute',
              left: offsetLeft + x - STRIP_HALF_WIDTH_PX,
              top: offsetTop,
              width: STRIP_HALF_WIDTH_PX * 2,
              height: drawHeight,
              cursor: 'ew-resize',
              backgroundImage: `linear-gradient(to bottom, ${LINE_COLOR} 50%, transparent 50%)`,
              backgroundSize: '1px 8px',
              backgroundRepeat: 'repeat-y',
              backgroundPosition: 'center',
              zIndex: 5,
            }}
          />
        )
      })}
      {/* Tiny chip with the label rendered above the strip when set,
           so users can scan all marker labels without hovering each. */}
      {markers.map((m) => {
        if (!m.label) return null
        const t = wallStartRef.current + m.ts_us / 1_000_000
        const x = plot.valToPos(t, 'x', false)
        if (x < 0 || x > drawWidth) return null
        return (
          <div
            key={`chip-${m.id}`}
            style={{
              position: 'absolute',
              left: offsetLeft + x + 4,
              top: offsetTop + 2,
              maxWidth: 180,
              padding: '1px 6px',
              fontSize: 11,
              lineHeight: '14px',
              background: 'rgba(100, 116, 139, 0.85)',
              color: 'white',
              borderRadius: 3,
              pointerEvents: 'none',
              whiteSpace: 'nowrap',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              zIndex: 5,
            }}
          >
            {m.label}
          </div>
        )
      })}
      {editingMarker != null && (
        <MarkerEditPopover
          marker={editingMarker}
          draftLabel={edit!.draftLabel}
          onChangeDraft={(v) =>
            setEdit({ markerId: editingMarker.id, draftLabel: v })
          }
          left={offsetLeft + editingX + 8}
          top={offsetTop + 8}
          maxLeft={offsetLeft + drawWidth - 280}
          onSave={() => {
            const next = edit!.draftLabel.trim() || null
            const orig = editingMarker.label ?? null
            if (next !== orig) {
              onLabelEdit?.(editingMarker.id, next)
            }
            setEdit(null)
          }}
          onCancel={() => setEdit(null)}
          onDelete={() => {
            onDelete?.(editingMarker.id)
            setEdit(null)
          }}
        />
      )}
    </>
  )
}

function MarkerEditPopover({
  marker,
  draftLabel,
  onChangeDraft,
  left,
  top,
  maxLeft,
  onSave,
  onCancel,
  onDelete,
}: {
  marker: Marker
  draftLabel: string
  onChangeDraft: (v: string) => void
  left: number
  top: number
  maxLeft: number
  onSave: () => void
  onCancel: () => void
  onDelete: () => void
}) {
  // Clamp horizontally so the popover doesn't run off the chart.
  const clampedLeft = Math.max(8, Math.min(left, maxLeft))
  return (
    <div
      style={{
        position: 'absolute',
        left: clampedLeft,
        top,
        width: 260,
        padding: 12,
        background: 'var(--color-bg-popup, white)',
        border: '1px solid var(--color-border-2, #ccc)',
        borderRadius: 6,
        boxShadow: '0 6px 24px rgba(0,0,0,0.15)',
        zIndex: 10,
        fontSize: 12,
      }}
      // Block clicks inside the popover from re-triggering chart cursor
      // logic via event bubbling.
      onMouseDown={(e) => e.stopPropagation()}
      onClick={(e) => e.stopPropagation()}
    >
      <div style={{ marginBottom: 8, color: 'var(--color-text-3)' }}>
        Marker @ {formatOffset(marker.ts_us)}
      </div>
      <input
        type="text"
        value={draftLabel}
        placeholder="Label (optional, e.g. 'enter battle')"
        onChange={(e) => onChangeDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') onSave()
          else if (e.key === 'Escape') onCancel()
        }}
        style={{
          width: '100%',
          padding: '6px 8px',
          border: '1px solid var(--color-border-2, #ccc)',
          borderRadius: 4,
          fontSize: 13,
          boxSizing: 'border-box',
          marginBottom: 10,
        }}
      />
      <div style={{ display: 'flex', justifyContent: 'space-between', gap: 8 }}>
        <button
          type="button"
          onClick={onDelete}
          style={popoverBtnStyle('danger')}
        >
          Delete
        </button>
        <div style={{ display: 'flex', gap: 6 }}>
          <button type="button" onClick={onCancel} style={popoverBtnStyle('plain')}>
            Cancel
          </button>
          <button type="button" onClick={onSave} style={popoverBtnStyle('primary')}>
            Save
          </button>
        </div>
      </div>
    </div>
  )
}

function popoverBtnStyle(kind: 'primary' | 'plain' | 'danger'): React.CSSProperties {
  const base: React.CSSProperties = {
    padding: '4px 12px',
    fontSize: 12,
    borderRadius: 4,
    cursor: 'pointer',
    border: '1px solid transparent',
  }
  if (kind === 'primary') {
    return { ...base, background: 'rgb(22, 93, 255)', color: 'white', borderColor: 'rgb(22, 93, 255)' }
  }
  if (kind === 'danger') {
    return { ...base, background: 'transparent', color: 'rgb(245, 63, 63)', borderColor: 'rgba(245, 63, 63, 0.5)' }
  }
  return { ...base, background: 'transparent', color: 'var(--color-text-2)', borderColor: 'var(--color-border-2, #ccc)' }
}

function formatOffset(tsUs: number): string {
  const totalMs = tsUs / 1000
  const m = Math.floor(totalMs / 60_000)
  const s = Math.floor((totalMs % 60_000) / 1000)
  const ms = Math.floor(totalMs % 1000)
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}.${String(ms).padStart(3, '0')}`
}
