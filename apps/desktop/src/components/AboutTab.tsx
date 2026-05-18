import { useQuery } from '@tanstack/react-query'
import { Typography } from '@arco-design/web-react'
import { getDiagnostics } from '@/lib/ipc'

const { Text, Title } = Typography

export function AboutTab() {
  const { data } = useQuery({
    queryKey: ['diagnostics'],
    queryFn: getDiagnostics,
    staleTime: 60_000,
  })

  return (
    <div style={{ padding: '8px 16px 16px' }}>
      <Title heading={6} style={{ margin: '4px 0 4px' }}>
        mperf
      </Title>
      <Text type="secondary" style={{ fontSize: 12 }}>
        v{data?.app_version ?? '—'} · Apache-2.0
      </Text>
      <div style={{ marginTop: 16, fontSize: 12, lineHeight: 1.7 }}>
        <Text>
          移动端性能采集工具，对标 Tencent PerfDog / Xcode Instruments。
          Android 走 adb；iOS 走纯 Rust 的 <code>idevice</code>（DTX / sysmontap），无
          Python、无 sudo、无 host daemon。
        </Text>
      </div>
      <div style={{ marginTop: 8 }}>
        <Text type="secondary" style={{ fontSize: 11 }}>
          mperf 是独立开发的开源项目，与 Tencent 无关联，未受其授权或赞助。PerfDog
          是腾讯公司的注册商标。
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
      <div style={{ marginTop: 16 }}>
        <Text type="secondary" style={{ fontSize: 11 }}>
          桌面端单机工具，数据不出本机。详细问题排查见 Settings 标签里的日志目录。
        </Text>
      </div>
    </div>
  )
}
