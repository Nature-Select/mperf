import { useQuery } from '@tanstack/react-query'
import { Button, Typography } from '@arco-design/web-react'
import { Copy, FolderOpen } from 'lucide-react'
import { open as openExternal } from '@tauri-apps/plugin-shell'
import { getDiagnostics, revealPath } from '@/lib/ipc'

const { Text, Title } = Typography

const REPO_URL = 'https://github.com/Nature-Select/mperf'

export function AboutTab() {
  const { data } = useQuery({
    queryKey: ['diagnostics'],
    queryFn: getDiagnostics,
    staleTime: 60_000,
  })

  // Tauri's webview captures plain anchor clicks — route external URLs
  // through the shell plugin so they actually land in the user's
  // default browser instead of replacing the app window.
  const onRepoClick = (e: React.MouseEvent) => {
    e.preventDefault()
    openExternal(REPO_URL).catch(() => {
      // Swallow — opening a URL is best-effort; nothing to fall back to.
    })
  }

  return (
    <div style={{ padding: '8px 16px 16px' }}>
      <Title heading={6} style={{ margin: '4px 0 4px' }}>
        mperf
      </Title>
      <Text type="secondary" style={{ fontSize: 12 }}>
        v{data?.app_version ?? '—'} · Apache-2.0
      </Text>
      <div style={{ marginTop: 8, fontSize: 12 }}>
        <a href={REPO_URL} onClick={onRepoClick} style={linkStyle}>
          {REPO_URL.replace(/^https?:\/\//, '')}
        </a>
      </div>
      <div style={{ marginTop: 16, fontSize: 12, lineHeight: 1.7 }}>
        <Text>
          移动端性能采集工具，对标 Tencent PerfDog / Xcode Instruments。
          Android 走 adb；iOS 走纯 Rust 的 <code>idevice</code>（DTX / sysmontap），无
          Python、无 sudo、无 host daemon。
        </Text>
      </div>
      <div style={{ marginTop: 16 }}>
        <Text bold style={{ fontSize: 12 }}>
          技术栈
        </Text>
        <ul style={{ marginTop: 6, paddingLeft: 18, fontSize: 12, lineHeight: 1.7 }}>
          <li>Tauri 2 · Rust workspace · tokio</li>
          <li>SQLite (WAL) · tokio-rusqlite</li>
          <li>React 19 · Arco Design · uPlot · TanStack Query</li>
          <li>
            adb sidecar（bundled）· idevice {data?.idevice_version ?? '—'}（iOS DTX）
          </li>
        </ul>
      </div>

      <div style={{ marginTop: 18 }}>
        <Text bold style={{ fontSize: 12 }}>
          诊断信息
        </Text>
        <div style={{ marginTop: 6 }}>
          <DiagRow label="OS" value={data?.os ?? '—'} />
          <DiagRow label="Schema" value={data ? `v${data.schema_version}` : '—'} />
          <DiagRow label="adb" value={data?.adb_version ?? '—'} mono />
          <DiagRow label="adb path" value={data?.adb_path ?? '—'} mono />
          <DiagRow label="Data dir" value={data?.data_dir ?? '—'} mono revealable />
          <DiagRow label="Log dir" value={data?.log_dir ?? '—'} mono revealable />
        </div>
      </div>

      <div style={{ marginTop: 16 }}>
        <Text type="secondary" style={{ fontSize: 11 }}>
          桌面端单机工具，数据不出本机。问题排查时把上方日志目录里的文件附上即可。
        </Text>
      </div>
    </div>
  )
}

interface DiagRowProps {
  label: string
  value: string
  /// Shows a "Reveal in Finder/Explorer" button next to Copy. Pass the
  /// file or directory path to navigate to.
  revealable?: boolean
  mono?: boolean
}

function DiagRow({ label, value, revealable, mono }: DiagRowProps) {
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
        gridTemplateColumns: '78px 1fr auto',
        gap: 8,
        alignItems: 'center',
        padding: '4px 0',
        borderBottom: '1px solid var(--color-border-1)',
      }}
    >
      <Text type="secondary" style={{ fontSize: 11 }}>
        {label}
      </Text>
      <Text
        style={{
          fontSize: 11,
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
            icon={<FolderOpen size={11} />}
            onClick={open}
            title="Open in file manager"
          />
        )}
        <Button size="mini" icon={<Copy size={11} />} onClick={copy} title="Copy" />
      </div>
    </div>
  )
}

const linkStyle: React.CSSProperties = {
  color: 'var(--color-link-default)',
  textDecoration: 'none',
  wordBreak: 'break-all',
}
