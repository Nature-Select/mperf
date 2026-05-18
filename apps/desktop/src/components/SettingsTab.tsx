import { useQuery } from '@tanstack/react-query'
import { Button, Spin, Typography } from '@arco-design/web-react'
import { Copy, FolderOpen } from 'lucide-react'
import { getDiagnostics, revealPath } from '@/lib/ipc'

const { Text } = Typography

interface RowProps {
  label: string
  value: string
  /// When set, shows a "Reveal in Finder/Explorer" button alongside Copy.
  /// Pass the same string as `value` (or the parent dir to navigate to).
  revealable?: boolean
  mono?: boolean
}

function Row({ label, value, revealable, mono }: RowProps) {
  const copy = () => {
    void navigator.clipboard.writeText(value)
  }
  const open = () => {
    void revealPath(value)
  }
  return (
    <div
      style={{
        display: 'grid',
        gridTemplateColumns: '110px 1fr auto',
        gap: 8,
        alignItems: 'center',
        padding: '6px 0',
        borderBottom: '1px solid var(--color-border-1)',
      }}
    >
      <Text type="secondary" style={{ fontSize: 12 }}>
        {label}
      </Text>
      <Text
        style={{
          fontSize: 12,
          fontFamily: mono ? 'ui-monospace, SFMono-Regular, monospace' : undefined,
          wordBreak: 'break-all',
          color: 'var(--color-text-1)',
        }}
      >
        {value}
      </Text>
      <div style={{ display: 'flex', gap: 4 }}>
        {revealable && (
          <Button
            size="mini"
            icon={<FolderOpen size={12} />}
            onClick={open}
            title="Open in file manager"
          />
        )}
        <Button size="mini" icon={<Copy size={12} />} onClick={copy} title="Copy" />
      </div>
    </div>
  )
}

export function SettingsTab() {
  const { data, isLoading, error } = useQuery({
    queryKey: ['diagnostics'],
    queryFn: getDiagnostics,
    staleTime: 60_000,
  })

  if (isLoading) {
    return (
      <div style={{ padding: 24, textAlign: 'center' }}>
        <Spin />
      </div>
    )
  }
  if (error || !data) {
    return (
      <div style={{ padding: 16 }}>
        <Text type="error" style={{ fontSize: 12 }}>
          诊断信息加载失败：{String(error)}
        </Text>
      </div>
    )
  }

  return (
    <div style={{ padding: '4px 16px 16px' }}>
      <Text
        type="secondary"
        style={{ fontSize: 11, display: 'block', marginBottom: 8 }}
      >
        诊断信息 · 当前没有可调配置项；本面板只读
      </Text>
      <Row label="App version" value={data.app_version} mono />
      <Row label="OS" value={data.os} />
      <Row label="Schema" value={`v${data.schema_version}`} />
      <Row label="idevice" value={data.idevice_version} mono />
      <Row label="adb" value={data.adb_version} mono />
      <Row label="adb path" value={data.adb_path} mono />
      <Row label="Data dir" value={data.data_dir} mono revealable />
      <Row label="Log dir" value={data.log_dir} mono revealable />
    </div>
  )
}
