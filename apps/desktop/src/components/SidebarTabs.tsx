import { ReactNode } from 'react'
import { Tabs, Typography } from '@arco-design/web-react'
import { Device } from '@/lib/ipc'
import { DeviceInfoPanel } from '@/components/DeviceInfoPanel'
import { SettingsTab } from '@/components/SettingsTab'
import { AboutTab } from '@/components/AboutTab'
import styles from './SidebarTabs.module.scss'

const { TabPane } = Tabs
const { Text } = Typography

/// Wraps each tab's body so longer panels (DeviceInfoPanel on Android
/// has 9+ rows) scroll inside the sider instead of pushing the whole
/// app layout.
function ScrollPane({ children }: { children: ReactNode }) {
  return (
    <div style={{ flex: 1, minHeight: 0, overflowY: 'auto' }}>{children}</div>
  )
}

export function SidebarTabs({ selected }: { selected: Device | null }) {
  return (
    <Tabs
      defaultActiveTab="device"
      size="small"
      className={styles.tabs}
      style={{ flex: 1, minHeight: 0, display: 'flex', flexDirection: 'column' }}
    >
      <TabPane key="device" title="设备">
        <ScrollPane>
          {selected ? (
            <DeviceInfoPanel
              deviceId={selected.id}
              platform={selected.platform}
              state={selected.state}
              transport={selected.transport}
              usable={selected.usable}
            />
          ) : (
            <div style={{ padding: 24, textAlign: 'center' }}>
              <Text type="secondary" style={{ fontSize: 12 }}>
                请在上方选择设备
              </Text>
            </div>
          )}
        </ScrollPane>
      </TabPane>
      <TabPane key="settings" title="设置">
        <ScrollPane>
          <SettingsTab />
        </ScrollPane>
      </TabPane>
      <TabPane key="about" title="关于">
        <ScrollPane>
          <AboutTab />
        </ScrollPane>
      </TabPane>
    </Tabs>
  )
}
