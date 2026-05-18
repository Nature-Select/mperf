import { useEffect, useState } from 'react'
import { Layout, Tabs, Typography } from '@arco-design/web-react'
import { LiveView } from '@/components/LiveView'
import { HistoryView } from '@/components/HistoryView'
import styles from './App.module.scss'

const { Header } = Layout
const { Title } = Typography
const TabPane = Tabs.TabPane

type TabKey = 'live' | 'history'

export default function App() {
  const [tab, setTab] = useState<TabKey>('live')
  /// Lifted to App so HistoryView can exclude the actively-recording
  /// session from its bulk-delete set. LiveView sets it on Start, clears
  /// it on Stop / disconnect / backend-side session-ended events.
  const [activeSessionId, setActiveSessionId] = useState<number | null>(null)

  // When a hidden tab becomes visible again, uPlot needs to resync its
  // pixel dimensions to the host (which was 0-width or stale while hidden).
  // Firing a window resize event is the cheapest way to trigger every
  // chart's existing onResize handler.
  useEffect(() => {
    const id = setTimeout(() => window.dispatchEvent(new Event('resize')), 0)
    return () => clearTimeout(id)
  }, [tab])

  return (
    <Layout style={{ height: '100vh' }}>
      <Header
        style={{
          height: 48,
          display: 'flex',
          alignItems: 'center',
          padding: '0 16px',
          borderBottom: '1px solid var(--color-border-2)',
          gap: 24,
        }}
      >
        <Title heading={6} style={{ margin: 0 }}>
          mperf
        </Title>
        <Tabs
          activeTab={tab}
          onChange={(k) => setTab(k as TabKey)}
          className={styles.topTabs}
          style={{ flex: 1 }}
        >
          <TabPane key="live" title="Live" />
          <TabPane key="history" title="History" />
        </Tabs>
      </Header>
      {/* Both views stay mounted; we only toggle visibility.
          Reasons: (1) LiveView holds the recording session state, which we
          must not lose when the user peeks at History; (2) chart instances
          keep their data buffers. */}
      <div
        style={{
          flex: 1,
          display: tab === 'live' ? 'flex' : 'none',
          minHeight: 0,
          minWidth: 0,
        }}
      >
        <LiveView
          activeSessionId={activeSessionId}
          setActiveSessionId={setActiveSessionId}
        />
      </div>
      <div
        style={{
          flex: 1,
          display: tab === 'history' ? 'block' : 'none',
          minHeight: 0,
          minWidth: 0,
        }}
      >
        <HistoryView activeSessionId={activeSessionId} />
      </div>
    </Layout>
  )
}
