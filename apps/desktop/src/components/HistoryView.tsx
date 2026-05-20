import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Button,
  Checkbox,
  List,
  Modal,
  Spin,
  Tag,
  Typography,
} from '@arco-design/web-react'
import { Trash2 } from 'lucide-react'
import { useState } from 'react'
import {
  CoreSampleRow,
  deleteMarker,
  deleteSession,
  getSessionCoreSamples,
  getSessionSamples,
  listSessionMarkers,
  listSessions,
  Marker,
  SamplePoint,
  SessionInfo,
  updateMarker,
  updateMarkerLabel,
} from '@/lib/ipc'
import { formatDateTime, formatDuration } from '@/lib/format'
import { MarkerControls } from '@/lib/markers'
import { useMetricsSelection } from '@/lib/useMetricsSelection'
import { StaticCpuChart } from './StaticCpuChart'
import { StaticPerCoreChart } from './StaticPerCoreChart'
import { StaticFpsChart } from './StaticFpsChart'
import { StaticGpuChart } from './StaticGpuChart'
import { StaticMemoryChart } from './StaticMemoryChart'
import { StaticTemperatureChart } from './StaticTemperatureChart'
import styles from './HistoryView.module.scss'

const { Text, Title } = Typography

export function HistoryView({
  activeSessionId,
}: {
  /// ID of the session currently being recorded, if any. We exclude it
  /// from the deletable set and disable its checkbox — deleting an
  /// active recording cascades pending sample writes into foreign-key
  /// errors and loses the in-flight data.
  activeSessionId: number | null
}) {
  const qc = useQueryClient()
  const { data: sessions, isLoading } = useQuery({
    queryKey: ['sessions'],
    queryFn: listSessions,
    refetchInterval: 5000,
  })

  const [selectedId, setSelectedId] = useState<number | null>(null)
  const [checked, setChecked] = useState<Set<number>>(new Set())
  const [confirmOpen, setConfirmOpen] = useState(false)
  const [deleting, setDeleting] = useState(false)
  const selected = sessions?.find((s) => s.id === selectedId) ?? null

  // Any recorded row is deletable, including in-progress / orphan rows
  // (e.g. a session whose USB disconnect happened before auto-finalize
  // landed). The currently-recording session is excluded so the user
  // can't pull the rug from under their own write stream.
  const deletable = (sessions ?? []).filter((s) => s.id !== activeSessionId)
  const allChecked = deletable.length > 0 && deletable.every((s) => checked.has(s.id))
  const indeterminate = !allChecked && deletable.some((s) => checked.has(s.id))

  const toggle = (id: number) => {
    setChecked((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  const toggleAll = () => {
    if (allChecked) setChecked(new Set())
    else setChecked(new Set(deletable.map((s) => s.id)))
  }

  const handleBulkDelete = () => {
    if (checked.size === 0) return
    setConfirmOpen(true)
  }

  const handleConfirmDelete = async () => {
    const ids = Array.from(checked)
    setDeleting(true)
    // allSettled (not all) so a single failure doesn't strand the rest of
    // the deletions in an unobservable state: the others may already have
    // succeeded server-side, and we still need to invalidate + close.
    const results = await Promise.allSettled(ids.map((id) => deleteSession(id)))
    const failed = results
      .map((r, i) => (r.status === 'rejected' ? ids[i] : null))
      .filter((x): x is number => x != null)
    const succeeded = ids.filter((id) => !failed.includes(id))
    if (failed.length > 0) {
      // Arco's Message is broken on React 19; without a banner system here,
      // the best we can do is log + leave only the failed rows still checked
      // so the user can see what didn't go through and retry.
      console.error('[mperf] bulk delete partial failure', { failed })
      setChecked(new Set(failed))
    } else {
      setChecked(new Set())
    }
    if (selectedId != null && succeeded.includes(selectedId)) setSelectedId(null)
    await qc.invalidateQueries({ queryKey: ['sessions'] })
    setConfirmOpen(false)
    setDeleting(false)
  }

  return (
    <div className={styles.root}>
      <div className={styles.sidebar}>
        <div className={styles.sidebarHeader}>
          <Text bold>Sessions</Text>
          <Text type="secondary" style={{ fontSize: 12 }}>
            {sessions?.length ?? 0} recorded
          </Text>
        </div>
        {deletable.length > 0 && (
          <div className={styles.bulkBar}>
            <Checkbox
              checked={allChecked}
              indeterminate={indeterminate}
              onChange={toggleAll}
            >
              <span style={{ fontSize: 12 }}>
                {checked.size > 0 ? `${checked.size} selected` : 'Select all'}
              </span>
            </Checkbox>
            <Button
              size="mini"
              type="text"
              status="danger"
              icon={<Trash2 size={12} />}
              disabled={checked.size === 0}
              onClick={handleBulkDelete}
            >
              Delete
            </Button>
          </div>
        )}
        {isLoading ? (
          <div className={styles.empty}>
            <Spin />
          </div>
        ) : (
          <List
            dataSource={sessions ?? []}
            noDataElement={
              <div className={styles.empty}>
                <Text type="secondary">No sessions yet. Record one in Live.</Text>
              </div>
            }
            render={(s: SessionInfo) => {
              const isActive = s.id === activeSessionId
              return (
              <List.Item
                key={s.id}
                className={selectedId === s.id ? styles.itemSelected : styles.item}
                onClick={() => setSelectedId(s.id)}
                actions={[
                  // The wrapping div stops the click from bubbling up to
                  // the List.Item's row navigation. Setting onClick on
                  // the Checkbox itself blocks onChange.
                  <div key="check" onClick={(e) => e.stopPropagation()}>
                    <Checkbox
                      checked={checked.has(s.id)}
                      disabled={isActive}
                      onChange={() => toggle(s.id)}
                    />
                  </div>,
                ]}
              >
                <List.Item.Meta
                  title={
                    <span>
                      {s.device_model || s.device_id}{' '}
                      <Tag size="small" color={s.device_platform === 'android' ? 'green' : 'arcoblue'}>
                        {s.device_platform}
                      </Tag>
                      {isActive ? (
                        <Tag size="small" color="red" style={{ marginLeft: 4 }}>
                          recording
                        </Tag>
                      ) : (
                        s.wall_end_ms == null && (
                          <Tag size="small" color="orange" style={{ marginLeft: 4 }}>
                            in progress
                          </Tag>
                        )
                      )}
                    </span>
                  }
                  description={
                    <div>
                      {s.device_model && (
                        <div
                          style={{
                            fontSize: 11,
                            color: 'var(--color-text-3)',
                            fontFamily: 'JetBrains Mono, SF Mono, Menlo, monospace',
                          }}
                        >
                          {s.device_id}
                        </div>
                      )}
                      <div>{formatDateTime(s.wall_start_ms)}</div>
                      <div style={{ fontSize: 11, color: 'var(--color-text-3)' }}>
                        {formatDuration(
                          s.wall_end_ms == null ? null : s.wall_end_ms - s.wall_start_ms,
                        )}
                      </div>
                    </div>
                  }
                />
              </List.Item>
              )
            }}
          />
        )}
      </div>
      <div className={styles.detail}>
        {selected ? <SessionDetail session={selected} /> : <EmptyDetail />}
      </div>
      <Modal
        title={`Delete ${checked.size} session${checked.size === 1 ? '' : 's'}?`}
        visible={confirmOpen}
        onOk={handleConfirmDelete}
        onCancel={() => setConfirmOpen(false)}
        okButtonProps={{ status: 'danger', loading: deleting }}
        okText="Delete"
        cancelText="Cancel"
      >
        <Text>This permanently removes the recorded samples.</Text>
      </Modal>
    </div>
  )
}

function EmptyDetail() {
  return (
    <div className={styles.emptyDetail}>
      <Text type="secondary">Select a session on the left to view its recording.</Text>
    </div>
  )
}

function SessionDetail({ session }: { session: SessionInfo }) {
  const qc = useQueryClient()
  // Mirrors LiveView's chart gating — we still capture every metric on
  // the backend (no schema or sampler changes), so flipping a metric
  // back on in the picker re-surfaces its chart here without needing
  // to re-record. Trades a small UX cost (you have to remember what
  // you toggled off) for retroactive data access, which is the right
  // default for performance debugging.
  const { selected: metricsSelection } = useMetricsSelection()
  const { data: total, isLoading: lt } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'cpu_total_pct'],
    queryFn: () => getSessionSamples(session.id, 'cpu_total_pct'),
  })
  const { data: appCpu } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'cpu_app_pct'],
    queryFn: () => getSessionSamples(session.id, 'cpu_app_pct'),
  })
  const { data: temps } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'cpu_temp_c'],
    queryFn: () => getSessionSamples(session.id, 'cpu_temp_c'),
  })
  const { data: batteryTemps } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'battery_temp_c'],
    queryFn: () => getSessionSamples(session.id, 'battery_temp_c'),
  })
  const { data: gpuDevice } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'gpu_device_pct'],
    queryFn: () => getSessionSamples(session.id, 'gpu_device_pct'),
  })
  const { data: gpuRenderer } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'gpu_renderer_pct'],
    queryFn: () => getSessionSamples(session.id, 'gpu_renderer_pct'),
  })
  const { data: gpuTiler } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'gpu_tiler_pct'],
    queryFn: () => getSessionSamples(session.id, 'gpu_tiler_pct'),
  })
  const { data: markers } = useQuery<Marker[]>({
    queryKey: ['markers', session.id],
    queryFn: () => listSessionMarkers(session.id),
  })
  // Drag-to-edit override — same pattern as App.tsx LiveView. While
  // dragging, every chart in History needs to render the marker at the
  // provisional position so they stay in sync.
  const [dragMarker, setDragMarker] = useState<{ id: number; ts_us: number } | null>(null)
  const markerList = (markers ?? []).map((m) =>
    dragMarker && m.id === dragMarker.id ? { ...m, ts_us: dragMarker.ts_us } : m,
  )
  const handleMarkerDragMove = (id: number, ts_us: number) =>
    setDragMarker({ id, ts_us })
  const handleMarkerDragEnd = (id: number, ts_us: number) => {
    setDragMarker(null)
    void updateMarker(id, ts_us)
      .then(() => qc.invalidateQueries({ queryKey: ['markers', session.id] }))
      .catch((e) => console.error('[mperf] update_marker failed', e))
  }
  const handleMarkerDelete = (id: number) => {
    void deleteMarker(id)
      .then(() => qc.invalidateQueries({ queryKey: ['markers', session.id] }))
      .catch((e) => console.error('[mperf] delete_marker failed', e))
  }
  const handleMarkerLabelEdit = (id: number, label: string | null) => {
    void updateMarkerLabel(id, label)
      .then(() => qc.invalidateQueries({ queryKey: ['markers', session.id] }))
      .catch((e) => console.error('[mperf] update_marker_label failed', e))
  }
  const markerControls: MarkerControls = {
    list: markerList,
    onDragMove: handleMarkerDragMove,
    onDragEnd: handleMarkerDragEnd,
    onDelete: handleMarkerDelete,
    onLabelEdit: handleMarkerLabelEdit,
  }
  const { data: cores, isLoading: lc } = useQuery<CoreSampleRow[]>({
    queryKey: ['core_samples', session.id],
    queryFn: () => getSessionCoreSamples(session.id),
  })
  const { data: fps, isLoading: lf } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'fps'],
    queryFn: () => getSessionSamples(session.id, 'fps'),
  })
  const { data: smallJanks } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'small_jank_count'],
    queryFn: () => getSessionSamples(session.id, 'small_jank_count'),
  })
  const { data: janks } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'jank_count'],
    queryFn: () => getSessionSamples(session.id, 'jank_count'),
  })
  const { data: bigJanks } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'big_jank_count'],
    queryFn: () => getSessionSamples(session.id, 'big_jank_count'),
  })
  const { data: stutters } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'stutter'],
    queryFn: () => getSessionSamples(session.id, 'stutter'),
  })
  const { data: pss } = useQuery<SamplePoint[]>({
    queryKey: ['samples', session.id, 'mem_app_pss_bytes'],
    queryFn: () => getSessionSamples(session.id, 'mem_app_pss_bytes'),
  })

  if (lt || lc || lf) {
    return (
      <div className={styles.emptyDetail}>
        <Spin />
      </div>
    )
  }

  const smallJankTotal = (smallJanks ?? []).reduce((a, p) => a + p.value, 0)
  const jankTotal = (janks ?? []).reduce((a, p) => a + p.value, 0)
  const bigJankTotal = (bigJanks ?? []).reduce((a, p) => a + p.value, 0)
  // Stutter is cumulative; the final emitted value is the session total.
  const stutterFinal =
    (stutters ?? []).length > 0 ? (stutters as SamplePoint[])[stutters!.length - 1].value : null

  const duration =
    session.wall_end_ms != null ? session.wall_end_ms - session.wall_start_ms : null

  return (
    <div>
      <Title heading={5} style={{ marginTop: 0, marginBottom: 4 }}>
        {session.device_model || session.device_id}
      </Title>
      <div className={styles.meta}>
        {session.device_model && (
          <>
            <span
              style={{
                fontFamily: 'JetBrains Mono, SF Mono, Menlo, monospace',
              }}
            >
              {session.device_id}
            </span>
            <span>·</span>
          </>
        )}
        <span>{formatDateTime(session.wall_start_ms)}</span>
        <span>·</span>
        <span>{formatDuration(duration)}</span>
        <span>·</span>
        <span>{total?.length ?? 0} samples</span>
      </div>
      {/*
        Chart order matches LiveView's picker-driven order
        (Frame → CPU Usage → CPU Core → Memory → GPU → Temperature) so
        scrolling Live and History feels like the same list. Each card
        is gated by BOTH (a) the user's current picker selection and
        (b) whether the session actually has data for this metric —
        toggling a metric back on in the picker resurfaces its chart
        without needing to re-record.
      */}
      {metricsSelection.has('frame') && (fps ?? []).length > 0 && (
        <StaticFpsChart
          fpsPoints={fps ?? []}
          smallJankTotal={Math.round(smallJankTotal)}
          jankTotal={Math.round(jankTotal)}
          bigJankTotal={Math.round(bigJankTotal)}
          stutter={stutterFinal}
          platform={session.device_platform === 'ios' ? 'ios' : 'android'}
          markers={markerControls}
          wallStartMs={session.wall_start_ms}
        />
      )}
      {metricsSelection.has('cpu_usage') && (
        <StaticCpuChart
          points={total ?? []}
          appPoints={appCpu ?? []}
          markers={markerControls}
          wallStartMs={session.wall_start_ms}
        />
      )}
      {metricsSelection.has('cpu_core') && (cores ?? []).length > 0 && (
        <StaticPerCoreChart
          rows={cores ?? []}
          markers={markerControls}
          wallStartMs={session.wall_start_ms}
        />
      )}
      {metricsSelection.has('memory') && (pss ?? []).length > 0 && (
        <StaticMemoryChart
          pssPoints={pss ?? []}
          markers={markerControls}
          wallStartMs={session.wall_start_ms}
        />
      )}
      {metricsSelection.has('gpu') &&
        ((gpuDevice ?? []).length > 0 ||
          (gpuRenderer ?? []).length > 0 ||
          (gpuTiler ?? []).length > 0) && (
          <StaticGpuChart
            devicePoints={gpuDevice ?? []}
            rendererPoints={gpuRenderer ?? []}
            tilerPoints={gpuTiler ?? []}
            markers={markerControls}
            wallStartMs={session.wall_start_ms}
          />
        )}
      {metricsSelection.has('temperature') &&
        ((temps ?? []).length > 0 || (batteryTemps ?? []).length > 0) && (
          <StaticTemperatureChart
            cpuPoints={temps ?? []}
            batteryPoints={batteryTemps ?? []}
            markers={markerControls}
            wallStartMs={session.wall_start_ms}
          />
        )}
    </div>
  )
}
