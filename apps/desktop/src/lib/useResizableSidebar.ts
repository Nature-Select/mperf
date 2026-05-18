import { useCallback, useEffect, useRef, useState } from 'react'

const STORAGE_KEY = 'mperf.sidebarWidth'
const MIN_WIDTH = 220
const MAX_WIDTH = 480
const DEFAULT_WIDTH = 280

function clamp(n: number): number {
  return Math.max(MIN_WIDTH, Math.min(MAX_WIDTH, n))
}

function readStored(): number {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (raw == null) return DEFAULT_WIDTH
    const n = Number(raw)
    return Number.isFinite(n) ? clamp(n) : DEFAULT_WIDTH
  } catch {
    return DEFAULT_WIDTH
  }
}

/// Sidebar width state with drag-to-resize on a handle element and
/// localStorage persistence. The handle's mousedown captures the
/// initial offset; mousemove updates width live; mouseup commits to
/// storage.
///
/// Returns:
///   - `width` — current pixel width to pass to `<Sider width={...} />`
///   - `handleProps` — spread onto the resize handle div
///   - `dragging` — true while the user is mid-drag (caller can swap
///     cursor / show overlay if needed)
export function useResizableSidebar() {
  const [width, setWidth] = useState<number>(() => readStored())
  const [dragging, setDragging] = useState(false)
  const dragStartXRef = useRef(0)
  const dragStartWidthRef = useRef(0)

  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      e.preventDefault()
      dragStartXRef.current = e.clientX
      dragStartWidthRef.current = width
      setDragging(true)
      ;(e.target as HTMLDivElement).setPointerCapture(e.pointerId)
    },
    [width],
  )

  const onPointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!dragging) return
      const delta = e.clientX - dragStartXRef.current
      const next = clamp(dragStartWidthRef.current + delta)
      setWidth(next)
    },
    [dragging],
  )

  const finishDrag = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!dragging) return
      setDragging(false)
      try {
        localStorage.setItem(STORAGE_KEY, String(width))
      } catch {
        // Quota exceeded or storage disabled — ignore; the width is
        // still effective for this session.
      }
      // Capture may already have been implicitly released (e.g. by a
      // pointercancel from the OS losing window focus); calling release
      // on an uncaptured pointer throws, so guard it.
      const el = e.target as HTMLDivElement
      if (el.hasPointerCapture?.(e.pointerId)) {
        el.releasePointerCapture(e.pointerId)
      }
    },
    [dragging, width],
  )

  // `pointercancel` fires when the OS interrupts the drag (window loses
  // focus, touch is hijacked by a system gesture, etc.). Without
  // routing it through the same finish path the sidebar gets stuck in
  // `dragging=true` until the next pointerup.
  const onPointerUp = finishDrag
  const onPointerCancel = finishDrag

  // Document-wide cursor + selection guard while dragging — prevents
  // text-selection flicker as the pointer crosses the chart area.
  useEffect(() => {
    if (!dragging) return
    const prevCursor = document.body.style.cursor
    const prevUserSelect = document.body.style.userSelect
    document.body.style.cursor = 'col-resize'
    document.body.style.userSelect = 'none'
    return () => {
      document.body.style.cursor = prevCursor
      document.body.style.userSelect = prevUserSelect
    }
  }, [dragging])

  return {
    width,
    dragging,
    handleProps: {
      onPointerDown,
      onPointerMove,
      onPointerUp,
      onPointerCancel,
    },
  }
}
