import { useQuery } from '@tanstack/react-query'
import { Spin, Typography } from '@arco-design/web-react'
import { Platform, Transport, getDeviceInfo } from '@/lib/ipc'
import styles from './DeviceInfoPanel.module.scss'

const { Text } = Typography

interface Props {
  deviceId: string
  platform: Platform
  /// Connection-state hint (e.g. "usb" / "network(...)"). Goes into the
  /// query key so wifi-only and USB queries are *separate* — when the
  /// user plugs in USB after selecting a wifi-only device, we want a
  /// fresh fetch instead of resurrecting the old (often hung) lockdown
  /// query. Optional for back-compat; falls back to keying on id alone.
  state?: string
  /// Transport currently in use. Shown as a "Connection" row at the
  /// top so the dropdown can stay clean (no inline USB/Wi-Fi badges).
  transport?: Transport
  /// Whether this entry can actually be sampled. iOS Wi-Fi is currently
  /// false (DTX needs USB tunnel) — we annotate the Connection row.
  usable?: boolean
}

const UNAVAILABLE = 'unavailable'

export function DeviceInfoPanel({ deviceId, platform, state, transport, usable }: Props) {
  const { data, isLoading, isError } = useQuery({
    queryKey: ['device_info', deviceId, platform, state ?? ''],
    queryFn: () => getDeviceInfo(deviceId, platform),
    // Device info is essentially static — cache across refetches.
    staleTime: 5 * 60_000,
  })

  if (isLoading) {
    return (
      <div className={styles.panel}>
        <Spin size={14} />
      </div>
    )
  }
  if (isError || !data) {
    return (
      <div className={styles.panel}>
        <Text type="secondary" style={{ fontSize: 12 }}>
          Could not fetch device info.
        </Text>
      </div>
    )
  }

  const connectionLabel = transport
    ? `${transport === 'usb' ? 'USB' : 'Wi-Fi'}${usable === false ? ' · 不可采样' : ''}`
    : null

  return (
    <div className={styles.panel}>
      <table className={styles.table}>
        <thead>
          <tr>
            <th className={styles.colInfo}>Info</th>
            <th className={styles.colValue}>Value</th>
          </tr>
        </thead>
        <tbody>
          {connectionLabel && <Row label="Connection" value={connectionLabel} />}
          {data.fields.map((f) => (
            <Row key={f.label} label={f.label} value={f.value} />
          ))}
        </tbody>
      </table>
    </div>
  )
}

function Row({ label, value }: { label: string; value: string | null }) {
  const isMissing = value === null || value === undefined || value === ''
  return (
    <tr>
      <td className={styles.cellInfo}>{label}</td>
      <td className={isMissing ? styles.cellValueMuted : styles.cellValue}>
        {isMissing ? UNAVAILABLE : value}
      </td>
    </tr>
  )
}
