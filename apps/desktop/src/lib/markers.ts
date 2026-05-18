import { Marker } from './ipc'

/// Bundled marker handlers passed from a session-level container
/// (`LiveView` / `HistoryView`) down through each chart to `MarkerOverlay`.
/// Collapses what used to be five separate props per chart — a chart
/// could silently lose `onDelete` or `onLabelEdit` if a caller forgot
/// to pass one, and that did happen (StaticCpuChart in HistoryView).
export type MarkerControls = {
  list: Marker[]
  onDragMove?: (id: number, tsUs: number) => void
  onDragEnd?: (id: number, tsUs: number) => void
  onDelete?: (id: number) => void
  onLabelEdit?: (id: number, label: string | null) => void
}
